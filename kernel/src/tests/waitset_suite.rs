//! Waiting on several endpoints and the raw input bus at once
//! (`channels::wait_any`, docs/performance-plan.md P1.3 and P1.4).
//!
//! The readiness rules are checked from the kernel task without parking. The
//! blocking side runs on a real kernel thread (`task::kthread`) that parks in
//! `wait_any` on endpoints opened in its own handle table, so every wake is a
//! real delivery, publication or timeout followed by a real context switch.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::input::bus;
use crate::ipc::channels::{self, harness as chan, Error as ChannelError, RAW_INPUT_READY};
use crate::ipc::handles;
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, WaitKind};

const ENDPOINTS: usize = channels::MAX_WAIT_ENDPOINTS;

/// The thread's endpoint handles (in its own table) and how many it waits on.
pub(super) static HANDLES: [AtomicU64; ENDPOINTS] = [const { AtomicU64::new(0) }; ENDPOINTS];
pub(super) static COUNT: AtomicUsize = AtomicUsize::new(0);
/// The thread's wait flags (doorbells, `WAIT_DEADLINE_NS`, `WAIT_FD`) and its
/// deadline (0: none).
pub(super) static RAW: AtomicU64 = AtomicU64::new(0);
pub(super) static DEADLINE: AtomicU64 = AtomicU64::new(0);
/// What each wait returned: the mask, or `u64::MAX - errno-ish` on error.
pub(super) static LAST: AtomicU64 = AtomicU64::new(0);
pub(super) static WAITS: AtomicU64 = AtomicU64::new(0);
/// `monotonic_ns` when the last wait returned, read in the thread itself.
pub(super) static RETURNED_NS: AtomicU64 = AtomicU64::new(0);
/// The thread parks here between rounds; the kernel task while it waits.
static GATE: WaitQueue = WaitQueue::new(WaitKind::Sleep);
pub(super) const TIMED_OUT: u64 = u64::MAX;
pub(super) const FAILED: u64 = u64::MAX - 1;

pub(super) fn parcel_bytes() -> Result<Vec<u8>, String> {
    let mut body = libmessenger::Encoder::new();
    body.u32(1, 7).map_err(|e| e.message())?;
    let parcel = libmessenger::Parcel {
        header: libmessenger::Header {
            version: libmessenger::VERSION,
            flags: libmessenger::flags::ONE_WAY,
            interface_id: 0x77,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).map_err(|e| e.message())?;
    Ok(bytes)
}

/// The waiting thread: one `wait_any` per round, result in `LAST`.
extern "C" fn waiter() -> ! {
    let me = task::current();
    loop {
        GATE.wait(me, None);
        let count = COUNT.load(Ordering::Relaxed);
        let handles: Vec<u64> = HANDLES[..count]
            .iter()
            .map(|h| h.load(Ordering::Relaxed))
            .collect();
        let deadline = match DEADLINE.load(Ordering::Relaxed) {
            0 => None,
            ticks => Some(ticks),
        };
        let doorbells = RAW.load(Ordering::Relaxed);
        let outcome = match channels::wait_any(&handles, doorbells, deadline) {
            Ok(mask) => mask,
            Err(ChannelError::TimedOut) => TIMED_OUT,
            Err(_) => FAILED,
        };
        RETURNED_NS.store(crate::arch::clock::monotonic_ns(), Ordering::Relaxed);
        LAST.store(outcome, Ordering::Relaxed);
        WAITS.fetch_add(1, Ordering::Relaxed);
    }
}

pub(super) struct Rig {
    pub(super) thread: usize,
    /// The kernel task's sending handles, one per thread endpoint.
    pub(super) senders: Vec<u64>,
}

/// A Realtime waiting thread with `count` endpoints in its own table.
pub(super) fn rig(count: usize) -> Result<Rig, String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    channels::reset();
    bus::reset();
    let thread = task::kthread::spawn_kernel_thread("waitset", waiter, PriorityClass::Realtime)
        .map_err(|e| format!("spawn: {e}"))?;
    handles::reset_for_task(thread);
    // A Realtime thread runs to its first park at the first yield.
    task::switch::yield_now();
    check!(GATE.contains(thread), "the thread did not park at its gate");
    let mut senders = Vec::new();
    for index in 0..count {
        let (sender, receiver) = channels::create().map_err(|e| format!("{e:?}"))?;
        let entry = handles::get(receiver).map_err(|e| format!("{e:?}"))?;
        let theirs = handles::open_for_task(thread, entry.kind, entry.rights, entry.object_id)
            .map_err(|e| format!("{e:?}"))?;
        HANDLES[index].store(theirs, Ordering::Relaxed);
        senders.push(sender);
    }
    COUNT.store(count, Ordering::Relaxed);
    RAW.store(0, Ordering::Relaxed);
    DEADLINE.store(0, Ordering::Relaxed);
    WAITS.store(0, Ordering::Relaxed);
    Ok(Rig { thread, senders })
}

impl Rig {
    /// Start one wait: the thread runs (it outranks the kernel task) until
    /// it parks in `wait_any`.
    pub(super) fn start_wait(&self) -> Result<(), String> {
        GATE.notify_one();
        task::preempt_point();
        check!(
            !GATE.contains(self.thread),
            "the thread did not leave its gate"
        );
        Ok(())
    }

    /// The result of the wait that just finished (the thread is back at its
    /// gate). Wakes are delivered through a preemption point, as a syscall
    /// return would.
    pub(super) fn finish_wait(&self, waits_before: u64) -> Result<u64, String> {
        task::preempt_point();
        check!(
            WAITS.load(Ordering::Relaxed) == waits_before + 1,
            "the wait did not return (waits {} -> {})",
            waits_before,
            WAITS.load(Ordering::Relaxed)
        );
        check!(
            GATE.contains(self.thread),
            "the thread is not back at its gate"
        );
        Ok(LAST.load(Ordering::Relaxed))
    }

    pub(super) fn teardown(self) -> Result<(), String> {
        task::harness::switch_current(task::KERNEL_TASK);
        channels::forget_task(self.thread);
        task::harness::finish(self.thread, 0);
        GATE.notify_all();
        while task::reap_child().is_some() {}
        let left = chan::total_waiters();
        channels::reset();
        bus::reset();
        task::harness::reset();
        check!(left == 0, "{left} endpoint registrations leaked");
        Ok(())
    }
}

/// Readiness without parking: queued messages and closed peers report at
/// once, in the right bits; bad sets are refused.
pub fn ready_masks() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    channels::reset();
    bus::reset();
    let message = parcel_bytes()?;
    let mut pairs = Vec::new();
    for _ in 0..4 {
        pairs.push(channels::create().map_err(|e| format!("{e:?}"))?);
    }
    let receivers: Vec<u64> = pairs.iter().map(|p| p.1).collect();
    channels::send(pairs[1].0, &message).map_err(|e| format!("{e:?}"))?;
    channels::send(pairs[3].0, &message).map_err(|e| format!("{e:?}"))?;
    let mask = channels::wait_any(&receivers, 0, None).map_err(|e| format!("{e:?}"))?;
    check!(mask == 0b1010, "queued messages gave mask {mask:#b}");
    channels::close_endpoint(pairs[0].0).map_err(|e| format!("{e:?}"))?;
    let mask = channels::wait_any(&receivers, 0, None).map_err(|e| format!("{e:?}"))?;
    check!(mask == 0b1011, "a closed peer is not ready: mask {mask:#b}");
    check!(
        chan::total_waiters() == 0,
        "a ready wait left registrations"
    );
    check!(
        channels::wait_any(&[], 0, None) == Err(ChannelError::BadParcel),
        "an empty set was accepted"
    );
    check!(
        channels::wait_any(&receivers[1..2], channels::WAIT_DOORBELLS + 1, None)
            == Err(ChannelError::BadParcel),
        "an unknown doorbell was accepted"
    );
    check!(
        channels::wait_any(&[], channels::WAIT_DISPLAY_KEYS, None) == Err(ChannelError::WrongKind),
        "the display doorbell was accepted from a task that does not own the display"
    );
    let nine = [receivers[1]; ENDPOINTS + 1];
    check!(
        channels::wait_any(&nine, 0, None) == Err(ChannelError::BadParcel),
        "an oversized set was accepted"
    );
    check!(
        channels::wait_any(&[0xdead], 0, None).is_err(),
        "a bad handle was accepted"
    );
    check!(
        channels::wait_any(&[], channels::WAIT_RAW_INPUT, None) == Err(ChannelError::WrongKind),
        "the raw bus was accepted without a consumer ring"
    );
    channels::reset();
    Ok(())
}

/// Parked on four endpoints, the thread wakes for a send to any one of them
/// with exactly that bit, and leaves no registration behind; a deadline
/// with nothing sent times out cleanly.
pub fn wakes_on_any() -> Result<(), String> {
    let rig = rig(4)?;
    let message = parcel_bytes()?;
    for target in [2usize, 0, 3, 1] {
        let before = WAITS.load(Ordering::Relaxed);
        rig.start_wait()?;
        check!(
            chan::total_waiters() == 4,
            "the parked thread holds {} registrations",
            chan::total_waiters()
        );
        channels::send(rig.senders[target], &message).map_err(|e| format!("{e:?}"))?;
        let mask = rig.finish_wait(before)?;
        check!(
            mask == 1 << target,
            "a send to {target} gave mask {mask:#b}"
        );
        check!(
            chan::total_waiters() == 0,
            "registrations survived the wake"
        );
        // Drain it as the thread would.
        task::harness::switch_current(rig.thread);
        let taken = channels::try_recv(HANDLES[target].load(Ordering::Relaxed));
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            matches!(taken, Ok(Some(_))),
            "the message was not there to take"
        );
    }
    // A deadline and nothing sent.
    DEADLINE.store(task::ticks() + 3, Ordering::Relaxed);
    let before = WAITS.load(Ordering::Relaxed);
    rig.start_wait()?;
    let started = task::ticks();
    while WAITS.load(Ordering::Relaxed) == before && task::ticks() < started + 50 {
        task::wait_sleep(task::ticks() + 1);
    }
    let mask = rig.finish_wait(before)?;
    check!(
        mask == TIMED_OUT,
        "an idle wait returned {mask:#x}, not a timeout"
    );
    rig.teardown()
}

/// The raw input doorbell: a publication wakes a consumer parked on the bus
/// (with no endpoint at all, and beside one), the records are there to
/// drain, and the doorbell is disarmed afterwards.
pub fn raw_input_doorbell() -> Result<(), String> {
    let rig = rig(1)?;
    let slot = bus::open(rig.thread).map_err(|e| format!("{e:?}"))?;
    RAW.store(1, Ordering::Relaxed);
    for (count, label) in [(0usize, "bus only"), (1, "bus and an endpoint")] {
        COUNT.store(count, Ordering::Relaxed);
        let before = WAITS.load(Ordering::Relaxed);
        rig.start_wait()?;
        bus::publish(bus::device::PS2_KEYBOARD, bus::kind::KEY, 30, 1);
        let mask = rig.finish_wait(before)?;
        check!(mask == RAW_INPUT_READY, "{label}: mask {mask:#x}");
        let mut out = Vec::new();
        bus::drain(slot, rig.thread, 16, &mut out).map_err(|e| format!("{e:?}"))?;
        check!(out.len() == 1, "{label}: {} records to drain", out.len());
    }
    // Records already waiting: the wait returns at once, without parking.
    bus::publish(bus::device::PS2_KEYBOARD, bus::kind::KEY, 30, 0);
    let before = WAITS.load(Ordering::Relaxed);
    GATE.notify_one();
    task::preempt_point();
    check!(
        WAITS.load(Ordering::Relaxed) == before + 1
            && LAST.load(Ordering::Relaxed) == RAW_INPUT_READY,
        "a waiting record did not satisfy the wait at once"
    );
    check!(
        bus::arm_doorbell(task::KERNEL_TASK).is_err(),
        "a task without a ring armed the doorbell"
    );
    rig.teardown()
}

/// The display key doorbell: a key pushed into the display input queue
/// wakes the owner parked with `WAIT_DISPLAY_KEYS`; a pointer event does not
/// (the compositor takes the pointer from `inputd`), and only the owner may
/// arm it.
pub fn display_key_doorbell() -> Result<(), String> {
    use crate::input::keyboard::Key;
    let rig = rig(1)?;
    crate::display::reset();
    crate::display::set_owner_for_test(rig.thread);
    RAW.store(channels::WAIT_DISPLAY_KEYS, Ordering::Relaxed);
    let before = WAITS.load(Ordering::Relaxed);
    rig.start_wait()?;
    crate::display::push_key(Key::Char('a'), true);
    let mask = rig.finish_wait(before)?;
    check!(
        mask == channels::DISPLAY_INPUT_READY,
        "a key gave mask {mask:#x}"
    );

    crate::display::reset();
    crate::display::set_owner_for_test(rig.thread);
    let before = WAITS.load(Ordering::Relaxed);
    rig.start_wait()?;
    crate::display::push_pointer_move(3, 4);
    task::preempt_point();
    check!(
        WAITS.load(Ordering::Relaxed) == before,
        "a pointer move rang the key doorbell"
    );
    channels::send(rig.senders[0], &parcel_bytes()?).map_err(|e| format!("{e:?}"))?;
    let mask = rig.finish_wait(before)?;
    check!(
        mask & 1 != 0,
        "the endpoint did not wake the parked owner: {mask:#x}"
    );
    check!(
        crate::display::arm_key_doorbell(task::KERNEL_TASK).is_err(),
        "a task that does not own the display armed its doorbell"
    );
    crate::display::reset();
    rig.teardown()
}

/// Soak: 100 000 rounds of a send to a pseudo-random one of eight
/// endpoints, interleaved with bus publications, each waking the parked
/// thread with exactly the right bit; no registration or queue entry leaks.
pub fn wait_any_soak() -> Result<(), String> {
    const ROUNDS: u64 = 100_000;
    let rig = rig(ENDPOINTS)?;
    let slot = bus::open(rig.thread).map_err(|e| format!("{e:?}"))?;
    RAW.store(1, Ordering::Relaxed);
    let message = parcel_bytes()?;
    let mut drained = Vec::new();
    for round in 0..ROUNDS {
        let before = WAITS.load(Ordering::Relaxed);
        rig.start_wait()?;
        let pick = ((round * 2_654_435_761) >> 7) as usize % (ENDPOINTS + 1);
        let expected = if pick == ENDPOINTS {
            bus::publish(
                bus::device::PS2_MOUSE,
                bus::kind::BUTTON,
                1,
                (round & 1) as i32,
            );
            RAW_INPUT_READY
        } else {
            channels::send(rig.senders[pick], &message).map_err(|e| format!("{e:?}"))?;
            1 << pick
        };
        let mask = rig.finish_wait(before)?;
        check!(
            mask == expected,
            "round {round}: mask {mask:#x}, expected {expected:#x}"
        );
        if pick == ENDPOINTS {
            drained.clear();
            bus::drain(slot, rig.thread, 16, &mut drained).map_err(|e| format!("{e:?}"))?;
        } else {
            task::harness::switch_current(rig.thread);
            let taken = channels::try_recv(HANDLES[pick].load(Ordering::Relaxed));
            task::harness::switch_current(task::KERNEL_TASK);
            check!(
                matches!(taken, Ok(Some(_))),
                "round {round}: nothing to take"
            );
        }
        check!(
            chan::total_waiters() == 0 && chan::queued_waiters() == 0,
            "round {round}: {} registrations, {} queue entries",
            chan::total_waiters(),
            chan::queued_waiters()
        );
    }
    serial_println!("TEST:ipc_waitset_soak:INFO:rounds={ROUNDS} endpoints={ENDPOINTS}");
    rig.teardown()
}

/// The `AF_INET` pump's doorbell (P4.1): a request queued by an application
/// wakes the attached `netd` parked on it (alone and beside an endpoint),
/// a ring while nobody waits is kept for the next wait, the bell is disarmed
/// afterwards, and only the attached task may arm it.
pub fn inet_doorbell() -> Result<(), String> {
    use crate::ipc::inet::{self, bell, Addr, Kind};
    let rig = rig(1)?;
    inet::reset();
    inet::attach(rig.thread);
    RAW.store(channels::WAIT_INET, Ordering::Relaxed);
    let peer = Addr {
        ip: [10, 0, 2, 2],
        port: 7,
    };
    for (count, label) in [(0usize, "bell only"), (1, "bell and an endpoint")] {
        COUNT.store(count, Ordering::Relaxed);
        let sock = inet::create(Kind::Stream).ok_or("no socket")?;
        let before = WAITS.load(Ordering::Relaxed);
        rig.start_wait()?;
        check!(
            bell::armed(),
            "{label}: the parked netd did not arm the bell"
        );
        sock.begin_connect(peer)
            .map_err(|e| format!("connect: {e}"))?;
        let mask = rig.finish_wait(before)?;
        check!(
            mask == channels::INET_READY,
            "{label}: connect gave {mask:#x}"
        );
        check!(!bell::armed(), "{label}: the bell stayed armed");
        // The close request rings while nobody waits: the next wait returns
        // at once.
        drop(sock);
        let before = WAITS.load(Ordering::Relaxed);
        GATE.notify_one();
        task::preempt_point();
        check!(
            WAITS.load(Ordering::Relaxed) == before + 1
                && LAST.load(Ordering::Relaxed) == channels::INET_READY,
            "{label}: a pending ring did not satisfy the next wait"
        );
        while inet::next_request().is_some() {}
    }
    check!(
        bell::arm(task::KERNEL_TASK).is_err(),
        "a task that is not the attached netd armed the bell"
    );
    check!(
        channels::wait_any(&[], channels::WAIT_INET, None) == Err(ChannelError::WrongKind),
        "the bell was accepted from a task that is not netd"
    );
    inet::reset();
    check!(
        bell::arm(rig.thread).is_err(),
        "a detached netd armed the bell"
    );
    rig.teardown()
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_waitset_ready_masks", ready_masks),
    ("ipc_waitset_wakes_on_any", wakes_on_any),
    ("ipc_waitset_raw_input_doorbell", raw_input_doorbell),
    ("ipc_waitset_display_key_doorbell", display_key_doorbell),
    ("ipc_waitset_inet_doorbell", inet_doorbell),
    ("ipc_waitset_soak", wait_any_soak),
];
