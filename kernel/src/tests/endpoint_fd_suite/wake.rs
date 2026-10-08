//! Wakeups: the keyed poll queue, a thread blocked in `epoll_wait`, and the
//! soaks (many endpoints, many messages, no leaked watch or waiter).

use core::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::task::wait::{WaitQueue, POLL};
use crate::task::{pollwait, PriorityClass, TaskState, WaitKind};

fn blocked(slot: usize) -> bool {
    matches!(task::harness::state(slot), Some(TaskState::Blocked { .. }))
}

/// The keyed-wakeup key of descriptor `fd`.
fn key_of(fd: u64) -> Result<u64, String> {
    let keys = task::fd_clone(fd as usize).and_then(|fd| fd.poll_keys());
    keys.map(|keys| keys[0])
        .ok_or_else(|| "an endpoint descriptor has no key".into())
}

/// Park `slot` on the poll queue as a `poll` whose scan saw only `key`.
fn park_on(slot: usize, key: u64) {
    task::harness::switch_current(slot);
    task::poll_scan_begin();
    pollwait::note_for_test(Some([key, 0]));
    task::harness::switch_current(task::KERNEL_TASK);
    POLL.park_ns(slot, None);
}

/// A delivery, a receive that frees room, a peer close and a released
/// handle each wake the waiters watching that endpoint, and only them.
pub fn keyed_wakeups() -> Result<(), String> {
    fresh()?;
    let waiter = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
    let (a, b) = pair()?;
    let (c, d) = pair()?;
    let fd = watch(b)?;
    let peer_fd = watch(a)?;
    let (key, peer_key) = (key_of(fd)?, key_of(peer_fd)?);
    check!(key != peer_key, "both sides share a key");
    let message = message()?;

    park_on(waiter, key);
    send(c, &message)?;
    check!(
        blocked(waiter),
        "a message on another channel woke the waiter"
    );
    send(a, &message)?;
    check!(!blocked(waiter), "a delivery did not wake the waiter");
    POLL.forget(waiter);

    park_on(waiter, peer_key);
    check!(take(b)?, "nothing to take");
    check!(
        !blocked(waiter),
        "room in the peer's way did not wake its writer"
    );
    POLL.forget(waiter);

    park_on(waiter, key);
    channels::close_endpoint(a).map_err(|e| format!("{e:?}"))?;
    check!(!blocked(waiter), "a peer close did not wake the waiter");
    POLL.forget(waiter);

    let entry = handles::get(b).map_err(|e| format!("{e:?}"))?;
    let other =
        handles::open(entry.kind, entry.rights, entry.object_id).map_err(|e| format!("{e:?}"))?;
    park_on(waiter, key);
    channels::release_endpoint(b).map_err(|e| format!("{e:?}"))?;
    check!(
        !blocked(waiter),
        "releasing the watched handle did not wake the waiter"
    );
    POLL.forget(waiter);

    for fd in [fd, peer_fd] {
        close(fd)?;
    }
    for handle in [other, c, d] {
        channels::close_endpoint(handle).map_err(|e| format!("{e:?}"))?;
    }
    task::harness::finish(waiter, 0);
    while task::reap_child().is_some() {}
    task::harness::reset();
    nothing_left("keyed_wakeups")
}

/// The polling thread's epoll descriptor (in its own table), its timeout
/// (ms; negative waits forever), what its last wait returned (the count,
/// and the first event's data) and how many waits it finished.
static EPFD: AtomicU64 = AtomicU64::new(0);
static TIMEOUT: AtomicU64 = AtomicU64::new(0);
static COUNT: AtomicU64 = AtomicU64::new(0);
static FIRST: AtomicU64 = AtomicU64::new(0);
static WAITS: AtomicU64 = AtomicU64::new(0);
/// The thread parks here between rounds.
static GATE: WaitQueue = WaitQueue::new(WaitKind::Sleep);

/// The polling thread: one `epoll_wait` per round.
extern "C" fn poller() -> ! {
    let me = task::current();
    loop {
        GATE.wait(me, None);
        let epfd = EPFD.load(Ordering::Relaxed);
        let timeout = TIMEOUT.load(Ordering::Relaxed) as i64;
        let (count, first) = match epoll_wait(epfd, timeout) {
            Ok(ready) => (ready.len() as u64, ready.first().map_or(u64::MAX, |e| e.1)),
            Err(_) => (u64::MAX, u64::MAX),
        };
        COUNT.store(count, Ordering::Relaxed);
        FIRST.store(first, Ordering::Relaxed);
        WAITS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Run `f` as `slot`, then as the kernel task again.
fn as_task<T>(slot: usize, f: impl FnOnce() -> T) -> T {
    task::harness::switch_current(slot);
    let out = f();
    task::harness::switch_current(task::KERNEL_TASK);
    out
}

/// A polling thread with `count` endpoints watched in one epoll set of its
/// own: `(thread, kernel-side senders, thread-side handles, thread fds)`.
struct Poller {
    thread: usize,
    senders: Vec<u64>,
    handles: Vec<u64>,
    fds: Vec<u64>,
}

fn poller_rig(count: usize) -> Result<Poller, String> {
    fresh()?;
    task::set_blocked(false);
    let thread = task::kthread::spawn_kernel_thread("epollfd", poller, PriorityClass::Realtime)
        .map_err(|e| format!("spawn: {e}"))?;
    handles::reset_for_task(thread);
    task::switch::yield_now();
    check!(GATE.contains(thread), "the thread did not park at its gate");
    let mut rig = Poller {
        thread,
        senders: Vec::new(),
        handles: Vec::new(),
        fds: Vec::new(),
    };
    let epfd = as_task(thread, epoll_create)?;
    EPFD.store(epfd, Ordering::Relaxed);
    for _ in 0..count {
        let (sender, receiver) = pair()?;
        let entry = handles::get(receiver).map_err(|e| format!("{e:?}"))?;
        let theirs = handles::open_for_task(thread, entry.kind, entry.rights, entry.object_id)
            .map_err(|e| format!("{e:?}"))?;
        handles::close(receiver).map_err(|e| format!("{e:?}"))?;
        let fd = as_task(thread, || -> Result<u64, String> {
            let fd = watch(theirs)?;
            epoll_add(epfd, fd, POLLIN as u32)?;
            Ok(fd)
        })?;
        rig.senders.push(sender);
        rig.handles.push(theirs);
        rig.fds.push(fd);
    }
    WAITS.store(0, Ordering::Relaxed);
    Ok(rig)
}

impl Poller {
    /// Start one `epoll_wait` with `timeout`: the thread runs until it parks.
    fn start(&self, timeout: i64) -> Result<u64, String> {
        TIMEOUT.store(timeout as u64, Ordering::Relaxed);
        let before = WAITS.load(Ordering::Relaxed);
        GATE.notify_one();
        task::preempt_point();
        check!(
            !GATE.contains(self.thread),
            "the thread did not leave its gate"
        );
        Ok(before)
    }

    /// The wait that just ended: `(count, first data)`.
    fn finish(&self, before: u64) -> Result<(u64, u64), String> {
        task::preempt_point();
        check!(
            WAITS.load(Ordering::Relaxed) == before + 1,
            "the epoll_wait did not return"
        );
        check!(
            GATE.contains(self.thread),
            "the thread is not back at its gate"
        );
        check!(
            !POLL.contains(self.thread),
            "the thread stayed on the poll queue"
        );
        Ok((COUNT.load(Ordering::Relaxed), FIRST.load(Ordering::Relaxed)))
    }

    /// Take one message from endpoint `index`, as the thread.
    fn drain(&self, index: usize) -> Result<bool, String> {
        as_task(self.thread, || take(self.handles[index]))
    }

    fn teardown(self) -> Result<(), String> {
        as_task(self.thread, || -> Result<(), String> {
            for &fd in &self.fds {
                close(fd)?;
            }
            close(EPFD.load(Ordering::Relaxed))
        })?;
        for &sender in &self.senders {
            channels::close_endpoint(sender).map_err(|e| format!("{e:?}"))?;
        }
        channels::forget_task(self.thread);
        task::harness::finish(self.thread, 0);
        GATE.notify_all();
        while task::reap_child().is_some() {}
        task::harness::reset();
        channels::reset();
        nothing_left("poller teardown")
    }
}

/// A thread blocked in `epoll_wait` wakes for a message on a watched
/// endpoint, and times out when nothing comes.
pub fn epoll_wait_wakes() -> Result<(), String> {
    let rig = poller_rig(2)?;
    let message = message()?;
    let before = rig.start(-1)?;
    check!(
        POLL.contains(rig.thread),
        "the thread is not parked in epoll_wait"
    );
    send(rig.senders[1], &message)?;
    let (count, first) = rig.finish(before)?;
    check!(
        count == 1 && first == rig.fds[1],
        "woke with {count} events, first {first}"
    );
    check!(rig.drain(1)?, "the message was lost");
    let before = rig.start(20)?;
    let give_up = crate::arch::clock::monotonic_ns() + 2_000_000_000;
    while WAITS.load(Ordering::Relaxed) == before && crate::arch::clock::monotonic_ns() < give_up {
        task::wait_sleep_ns(crate::arch::clock::monotonic_ns() + 1_000_000);
    }
    let (count, _) = rig.finish(before)?;
    check!(count == 0, "a quiet wait returned {count} events");
    rig.teardown()
}

/// Soak, from the kernel task: 16 endpoints in one level-triggered epoll set
/// and 20 000 random sends, receives and descriptor reopenings; every
/// `epoll_wait` reports exactly the endpoints holding a message.
pub fn epoll_soak() -> Result<(), String> {
    const ENDPOINTS: usize = 16;
    const ROUNDS: u32 = 20_000;
    fresh()?;
    let message = message()?;
    let epfd = epoll_create()?;
    let mut ends = Vec::new();
    for _ in 0..ENDPOINTS {
        let (sender, receiver) = pair()?;
        let fd = watch(receiver)?;
        epoll_add(epfd, fd, POLLIN as u32)?;
        ends.push((sender, receiver, fd));
    }
    let mut queued = [0u32; ENDPOINTS];
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let (mut sends, mut takes, mut reopens) = (0u64, 0u64, 0u64);
    for round in 0..ROUNDS {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let index = (seed % ENDPOINTS as u64) as usize;
        match (seed >> 32) % 9 {
            0..=4 if queued[index] < 8 => {
                send(ends[index].0, &message)?;
                queued[index] += 1;
                sends += 1;
            }
            8 => {
                // A fresh descriptor for the same endpoint.
                let (_, receiver, fd) = ends[index];
                close(fd)?;
                let fd = watch(receiver)?;
                epoll_add(epfd, fd, POLLIN as u32)?;
                ends[index].2 = fd;
                reopens += 1;
            }
            _ if queued[index] > 0 => {
                check!(
                    take(ends[index].1)?,
                    "round {round}: endpoint {index} was empty"
                );
                queued[index] -= 1;
                takes += 1;
            }
            _ => {}
        }
        let mut ready: Vec<u64> = epoll_wait(epfd, 0)?.iter().map(|&(_, fd)| fd).collect();
        ready.sort_unstable();
        let mut expected: Vec<u64> = (0..ENDPOINTS)
            .filter(|&i| queued[i] > 0)
            .map(|i| ends[i].2)
            .collect();
        expected.sort_unstable();
        check!(
            ready == expected,
            "round {round}: epoll saw {ready:?}, expected {expected:?}"
        );
        check!(
            endpointfd::live() == ENDPOINTS,
            "round {round}: {} watches for {ENDPOINTS} descriptors",
            endpointfd::live()
        );
    }
    for (sender, receiver, fd) in ends {
        close(fd)?;
        channels::close_endpoint(sender).map_err(|e| format!("{e:?}"))?;
        channels::close_endpoint(receiver).map_err(|e| format!("{e:?}"))?;
    }
    close(epfd)?;
    serial_println!(
        "TEST:ipc_endpoint_fd_epoll_soak:INFO:rounds={ROUNDS} sends={sends} takes={takes} \
         reopens={reopens}"
    );
    nothing_left("epoll_soak")
}

/// Soak with real blocking: 2 000 rounds of a thread parked in
/// `epoll_wait` over 8 endpoints, woken by a send to a random one (or, one
/// round in eight, by its own 2 ms timeout); every wake names the right
/// endpoint and nothing stays parked or registered.
pub fn blocking_soak() -> Result<(), String> {
    const ROUNDS: u64 = 2_000;
    let rig = poller_rig(8)?;
    let message = message()?;
    let (mut wakes, mut timeouts) = (0u64, 0u64);
    for round in 0..ROUNDS {
        let index = ((round * 2_654_435_761) >> 7) as usize % rig.fds.len();
        if round % 8 == 7 {
            let before = rig.start(2)?;
            let give_up = crate::arch::clock::monotonic_ns() + 2_000_000_000;
            while WAITS.load(Ordering::Relaxed) == before
                && crate::arch::clock::monotonic_ns() < give_up
            {
                task::wait_sleep_ns(crate::arch::clock::monotonic_ns() + 200_000);
            }
            let (count, _) = rig.finish(before)?;
            check!(count == 0, "round {round}: a quiet wait returned {count}");
            timeouts += 1;
            continue;
        }
        let before = rig.start(-1)?;
        send(rig.senders[index], &message)?;
        let (count, first) = rig.finish(before)?;
        check!(
            count == 1 && first == rig.fds[index],
            "round {round}: {count} events, first {first}, expected fd {}",
            rig.fds[index]
        );
        check!(rig.drain(index)?, "round {round}: the message was lost");
        wakes += 1;
    }
    check!(chan::total_waiters() == 0, "endpoint registrations leaked");
    serial_println!(
        "TEST:ipc_endpoint_fd_blocking_soak:INFO:rounds={ROUNDS} wakes={wakes} timeouts={timeouts}"
    );
    rig.teardown()
}
