//! Waiting on pending calls beside endpoints (`channels::wait_any` with
//! `WAIT_ITEM_CALL`, issue #309; docs/architecture/wait-any.md).
//!
//! A kernel thread plays the client: each round it begins its calls on
//! endpoints in its own handle table (so it is their caller), parks once in
//! `wait_any` on those calls and one endpoint, then finishes every call (a
//! ready one is awaited, which must not park; the rest are canceled first).
//! The kernel task plays the server: it takes the requests and replies,
//! sends, closes or lets a deadline pass, and checks the mask and each call's
//! outcome. Every round must leave no endpoint registration, no queue entry
//! and no transaction behind.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::ipc::channels::{self, harness as chan, Error as ChannelError, WAIT_ITEM_CALL};
use crate::ipc::handles;
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, WaitKind};

/// Calls the thread has in flight per round (one more item is its endpoint).
const CALLS: usize = channels::MAX_WAIT_ENDPOINTS - 1;

/// The thread's client handles (its table), one per server channel.
static CLIENTS: [AtomicU64; CALLS] = [const { AtomicU64::new(0) }; CALLS];
/// How many calls it begins this round, and their deadline (0: none).
static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
static CALL_DEADLINE: AtomicU64 = AtomicU64::new(0);
/// The thread's one-way inbox handle (0: no endpoint in the set).
static INBOX: AtomicU64 = AtomicU64::new(0);
/// A transaction someone else began, put in the set instead of the calls.
static FOREIGN: AtomicU64 = AtomicU64::new(0);
/// The wait's own deadline (0: none).
static DEADLINE: AtomicU64 = AtomicU64::new(0);
/// The mask the wait returned, or an error code below.
static LAST: AtomicU64 = AtomicU64::new(0);
/// Each call's outcome after the round.
static OUTCOMES: [AtomicU64; CALLS] = [const { AtomicU64::new(0) }; CALLS];
static WAITS: AtomicU64 = AtomicU64::new(0);
static GATE: WaitQueue = WaitQueue::new(WaitKind::Sleep);

const TIMED_OUT: u64 = u64::MAX;
const NOT_CALLER: u64 = u64::MAX - 1;
const NO_TXN: u64 = u64::MAX - 2;
const FAILED: u64 = u64::MAX - 3;
/// Call outcomes.
const REPLIED: u64 = 1;
const CANCELED: u64 = 2;
const EXPIRED: u64 = 3;
const PEER_DIED: u64 = 4;
const OTHER: u64 = 5;

fn code_of(error: ChannelError) -> u64 {
    match error {
        ChannelError::TimedOut => TIMED_OUT,
        ChannelError::NotCaller => NOT_CALLER,
        ChannelError::NoTransaction => NO_TXN,
        _ => FAILED,
    }
}

fn outcome_of(result: Result<Vec<u8>, ChannelError>) -> u64 {
    match result {
        Ok(_) => REPLIED,
        Err(ChannelError::Canceled) => CANCELED,
        Err(ChannelError::TimedOut) => EXPIRED,
        Err(ChannelError::PeerDied) => PEER_DIED,
        Err(_) => OTHER,
    }
}

/// One round of the client thread.
fn client_round() {
    let calls = CALL_COUNT.load(Ordering::Relaxed);
    let call_deadline = match CALL_DEADLINE.load(Ordering::Relaxed) {
        0 => None,
        ticks => Some(ticks),
    };
    let request = super::waitset_suite::parcel_bytes().unwrap_or_default();
    let mut txns = Vec::new();
    for client in &CLIENTS[..calls] {
        match channels::begin_call(client.load(Ordering::Relaxed), 1, &request, call_deadline) {
            Ok(txn) => txns.push(txn),
            Err(_) => {
                LAST.store(FAILED, Ordering::Relaxed);
                return;
            }
        }
    }
    let mut words: Vec<u64> = txns.iter().map(|txn| txn | WAIT_ITEM_CALL).collect();
    match FOREIGN.load(Ordering::Relaxed) {
        0 => {}
        txn => words.push(txn | WAIT_ITEM_CALL),
    }
    let inbox = INBOX.load(Ordering::Relaxed);
    if inbox != 0 {
        words.push(inbox);
    }
    let deadline = match DEADLINE.load(Ordering::Relaxed) {
        0 => None,
        ticks => Some(ticks),
    };
    let result = channels::wait_any(&words, 0, deadline);
    let mask = result.unwrap_or(0);
    LAST.store(result.unwrap_or_else(code_of), Ordering::Relaxed);
    for (index, &txn) in txns.iter().enumerate() {
        if mask & (1 << index) == 0 {
            let _ = channels::cancel(txn);
        }
        OUTCOMES[index].store(outcome_of(channels::await_reply(txn)), Ordering::Relaxed);
    }
}

extern "C" fn client() -> ! {
    let me = task::current();
    loop {
        GATE.wait(me, None);
        client_round();
        WAITS.fetch_add(1, Ordering::Relaxed);
    }
}

struct Rig {
    thread: usize,
    /// The kernel task's server handles, one per client handle.
    servers: Vec<u64>,
    /// The kernel task's end that sends to the thread's inbox.
    sender: u64,
}

/// Give `thread` its own handle to the endpoint `handle` names.
fn share(thread: usize, handle: u64) -> Result<u64, String> {
    let entry = handles::get(handle).map_err(|e| format!("{e:?}"))?;
    handles::open_for_task(thread, entry.kind, entry.rights, entry.object_id)
        .map_err(|e| format!("{e:?}"))
}

fn rig() -> Result<Rig, String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    channels::reset();
    let thread = task::kthread::spawn_kernel_thread("waitcall", client, PriorityClass::Realtime)
        .map_err(|e| format!("spawn: {e}"))?;
    handles::reset_for_task(thread);
    task::switch::yield_now();
    check!(GATE.contains(thread), "the thread did not park at its gate");
    let mut servers = Vec::new();
    for client in &CLIENTS {
        let (mine, theirs) = channels::create().map_err(|e| format!("{e:?}"))?;
        client.store(share(thread, theirs)?, Ordering::Relaxed);
        servers.push(mine);
    }
    let (sender, inbox) = channels::create().map_err(|e| format!("{e:?}"))?;
    INBOX.store(share(thread, inbox)?, Ordering::Relaxed);
    CALL_COUNT.store(CALLS, Ordering::Relaxed);
    CALL_DEADLINE.store(0, Ordering::Relaxed);
    FOREIGN.store(0, Ordering::Relaxed);
    DEADLINE.store(0, Ordering::Relaxed);
    WAITS.store(0, Ordering::Relaxed);
    Ok(Rig {
        thread,
        servers,
        sender,
    })
}

impl Rig {
    /// Run the thread until it parks in `wait_any` (or finishes a round that
    /// did not park). Returns the wait count before the round.
    fn start(&self) -> u64 {
        let before = WAITS.load(Ordering::Relaxed);
        GATE.notify_one();
        task::preempt_point();
        before
    }

    /// Take the request queued on server `index`: its transaction id.
    fn take_request(&self, index: usize) -> Result<u64, String> {
        match channels::try_recv(self.servers[index]) {
            Ok(Some(message)) => message.txn.ok_or_else(|| "a one-way request".into()),
            other => Err(format!("server {index}: no request ({other:?})")),
        }
    }

    /// The finished round's mask (the thread is back at its gate).
    fn finish(&self, before: u64) -> Result<u64, String> {
        task::preempt_point();
        check!(
            WAITS.load(Ordering::Relaxed) == before + 1,
            "the round did not finish"
        );
        Ok(LAST.load(Ordering::Relaxed))
    }

    /// Wait (sleeping, ticks pass) for a round that ends by a deadline.
    fn finish_after_deadline(&self, before: u64) -> Result<u64, String> {
        let started = task::ticks();
        while WAITS.load(Ordering::Relaxed) == before && task::ticks() < started + 100 {
            task::wait_sleep(task::ticks() + 1);
        }
        self.finish(before)
    }

    fn outcome(&self, index: usize) -> u64 {
        OUTCOMES[index].load(Ordering::Relaxed)
    }

    fn teardown(self) -> Result<(), String> {
        task::harness::switch_current(task::KERNEL_TASK);
        channels::forget_task(self.thread);
        task::harness::finish(self.thread, 0);
        GATE.notify_all();
        while task::reap_child().is_some() {}
        let left = chan::total_waiters();
        channels::reset();
        task::harness::reset();
        check!(left == 0, "{left} endpoint registrations leaked");
        Ok(())
    }
}

/// No round may leave a registration, queue entry or transaction behind.
fn clean(label: &str) -> Result<(), String> {
    check!(
        chan::total_waiters() == 0 && chan::queued_waiters() == 0 && chan::live_transactions() == 0,
        "{label}: {} registrations, {} queue entries, {} transactions left",
        chan::total_waiters(),
        chan::queued_waiters(),
        chan::live_transactions()
    );
    Ok(())
}

/// A reply to any one of the in-flight calls wakes the thread with exactly
/// that call's bit (not the oldest call's), the reply is there to await,
/// and a send to the endpoint in the same set wakes it too.
pub fn wakes_on_reply() -> Result<(), String> {
    let rig = rig()?;
    let reply = super::waitset_suite::parcel_bytes()?;
    for target in [3usize, 0, CALLS - 1, 1] {
        let before = rig.start();
        let mut txns = Vec::new();
        for index in 0..CALLS {
            txns.push(rig.take_request(index)?);
        }
        channels::reply(txns[target], &reply).map_err(|e| format!("{e:?}"))?;
        let mask = rig.finish(before)?;
        check!(mask == 1 << target, "a reply to {target} gave {mask:#b}");
        for index in 0..CALLS {
            let want = if index == target { REPLIED } else { CANCELED };
            check!(
                rig.outcome(index) == want,
                "call {index}: outcome {}",
                rig.outcome(index)
            );
        }
        clean("reply")?;
    }
    let before = rig.start();
    channels::send(rig.sender, &reply).map_err(|e| format!("{e:?}"))?;
    let mask = rig.finish(before)?;
    check!(mask == 1 << CALLS, "a send gave {mask:#b}");
    for index in 0..CALLS {
        let _ = rig.take_request(index);
    }
    task::harness::switch_current(rig.thread);
    let taken = channels::try_recv(INBOX.load(Ordering::Relaxed));
    task::harness::switch_current(task::KERNEL_TASK);
    check!(matches!(taken, Ok(Some(_))), "the message was not there");
    clean("send")?;
    rig.teardown()
}

/// A call's own deadline wakes a wait with none (the call is expired and
/// ready); a wait deadline earlier than the call's ends the wait with
/// `TimedOut` and the call still pending; a canceled call is ready at once.
pub fn call_deadlines() -> Result<(), String> {
    let rig = rig()?;
    CALL_COUNT.store(1, Ordering::Relaxed);
    CALL_DEADLINE.store(task::ticks() + 3, Ordering::Relaxed);
    let before = rig.start();
    let mask = rig.finish_after_deadline(before)?;
    check!(mask == 1, "an expiring call gave {mask:#x}");
    check!(rig.outcome(0) == EXPIRED, "outcome {}", rig.outcome(0));
    let _ = rig.take_request(0);
    clean("call deadline")?;

    CALL_DEADLINE.store(task::ticks() + 500, Ordering::Relaxed);
    DEADLINE.store(task::ticks() + 3, Ordering::Relaxed);
    let before = rig.start();
    let mask = rig.finish_after_deadline(before)?;
    check!(mask == TIMED_OUT, "the wait deadline gave {mask:#x}");
    check!(rig.outcome(0) == CANCELED, "outcome {}", rig.outcome(0));
    let _ = rig.take_request(0);
    clean("wait deadline")?;
    rig.teardown()
}

/// Peer death ends a waited call; another task's call and an unknown one
/// are refused without parking.
pub fn peer_death_and_refusals() -> Result<(), String> {
    let rig = rig()?;
    let before = rig.start();
    channels::close_endpoint(rig.servers[2]).map_err(|e| format!("{e:?}"))?;
    let mask = rig.finish(before)?;
    check!(mask == 1 << 2, "a closed server gave {mask:#b}");
    check!(rig.outcome(2) == PEER_DIED, "outcome {}", rig.outcome(2));
    clean("peer death")?;

    // The kernel task's own call, named by the thread.
    CALL_COUNT.store(0, Ordering::Relaxed);
    INBOX.store(0, Ordering::Relaxed);
    let (client, server) = channels::create().map_err(|e| format!("{e:?}"))?;
    let request = super::waitset_suite::parcel_bytes()?;
    let txn = channels::begin_call(client, 1, &request, None).map_err(|e| format!("{e:?}"))?;
    FOREIGN.store(txn, Ordering::Relaxed);
    let before = rig.start();
    check!(
        rig.finish(before)? == NOT_CALLER,
        "another task's call was accepted"
    );
    FOREIGN.store(txn ^ (1 << 40), Ordering::Relaxed);
    let before = rig.start();
    check!(
        rig.finish(before)? == NO_TXN,
        "an unknown call was accepted"
    );
    // A canceled call of the caller's own is ready at once.
    channels::cancel(txn).map_err(|e| format!("{e:?}"))?;
    let mask = channels::wait_any(&[txn | WAIT_ITEM_CALL], 0, None);
    check!(mask == Ok(1), "a canceled call gave {mask:?}");
    check!(
        channels::await_reply(txn) == Err(ChannelError::Canceled),
        "the canceled call did not report Canceled"
    );
    let _ = channels::try_recv(server);
    clean("refusals")?;
    rig.teardown()
}

/// Soak: 20 000 rounds of seven calls plus an endpoint, each woken by a
/// reply to a pseudo-random call or a send; no registration, queue entry or
/// transaction leaks across the generations.
pub fn soak() -> Result<(), String> {
    const ROUNDS: u64 = 20_000;
    let rig = rig()?;
    let reply = super::waitset_suite::parcel_bytes()?;
    for round in 0..ROUNDS {
        let before = rig.start();
        let mut txns = Vec::new();
        for index in 0..CALLS {
            txns.push(rig.take_request(index)?);
        }
        let pick = ((round * 2_654_435_761) >> 7) as usize % (CALLS + 1);
        if pick == CALLS {
            channels::send(rig.sender, &reply).map_err(|e| format!("{e:?}"))?;
        } else {
            channels::reply(txns[pick], &reply).map_err(|e| format!("{e:?}"))?;
        }
        let mask = rig.finish(before)?;
        check!(
            mask == 1 << pick,
            "round {round}: mask {mask:#b}, pick {pick}"
        );
        if pick == CALLS {
            task::harness::switch_current(rig.thread);
            let taken = channels::try_recv(INBOX.load(Ordering::Relaxed));
            task::harness::switch_current(task::KERNEL_TASK);
            check!(matches!(taken, Ok(Some(_))), "round {round}: no message");
        }
        clean("soak")?;
    }
    serial_println!("TEST:ipc_waitcall_soak:INFO:rounds={ROUNDS} calls={CALLS}");
    rig.teardown()
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_waitcall_wakes_on_reply", wakes_on_reply),
    ("ipc_waitcall_deadlines", call_deadlines),
    ("ipc_waitcall_peer_death_refusals", peer_death_and_refusals),
    ("ipc_waitcall_soak", soak),
];
