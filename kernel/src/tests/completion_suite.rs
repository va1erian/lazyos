//! The per-task completion queue for calls (issue #725,
//! docs/architecture/call-completion.md).
//!
//! A kernel thread plays a client that never parks on `await_reply`: each
//! round it begins calls, parks once in `wait_any` with the `WAIT_CALLS`
//! doorbell alone, drains the finished transactions and checks each outcome
//! with `await_reply_now` (which must not park). The kernel task plays the
//! server and ends the calls in every way there is: a reply, the call's own
//! deadline, a cancel the client issues itself, and the server closing its
//! side. Every ended call is reported exactly once, and every round must
//! leave no queue entry, no transaction and no endpoint registration behind.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::ipc::channels::{self, harness as chan, Error as ChannelError, CALLS_READY, WAIT_CALLS};
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, WaitKind};

/// Calls the thread begins per round.
const CALLS: usize = 2;

/// How many calls the thread begins this round and their deadline (0: none).
static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
static CALL_DEADLINE: AtomicU64 = AtomicU64::new(0);
/// The round's outcome per call, as the client checked it.
static OUTCOMES: [AtomicU64; CALLS] = [const { AtomicU64::new(0) }; CALLS];
/// Finished transaction ids the client's drains reported (soak invariant:
/// never the same one twice within a round), and how many.
static REPORTED: AtomicU64 = AtomicU64::new(0);
/// Completed rounds.
static ROUNDS: AtomicU64 = AtomicU64::new(0);
/// The client's handle per server channel.
static CLIENTS: [AtomicU64; CALLS] = [const { AtomicU64::new(0) }; CALLS];
/// Stage the client round reached, for a hung round's report: 1 begins ok,
/// 2 the wait refused, 3 the mask missed the doorbell bit, 4 the drains
/// ran, 5 done; 9/10/11 the failed begin/drain/tail paths.
static STAGE: AtomicU64 = AtomicU64::new(0);
/// The wait's own mask, when the round got one.
static MASK: AtomicU64 = AtomicU64::new(0);

static GATE: WaitQueue = WaitQueue::new(WaitKind::Sleep);

const TIMED_OUT: u64 = u64::MAX;
const FAILED: u64 = u64::MAX - 1;
const NO_TXN: u64 = u64::MAX - 2;
/// Round outcomes.
const REPLIED: u64 = 1;
const CANCELED: u64 = 2;
const EXPIRED: u64 = 3;
const PEER_DIED: u64 = 4;

fn code_of(error: ChannelError) -> u64 {
    match error {
        ChannelError::TimedOut => TIMED_OUT,
        ChannelError::NoTransaction => NO_TXN,
        ChannelError::Canceled => CANCELED,
        _ => FAILED,
    }
}

/// One round of the client thread: begin, park on `WAIT_CALLS`, take the
/// completions, finish every call (a still-open one is canceled, the way a
/// completion-driven loop drops a request it gives up on), and take the
/// completions the tail produced.
fn client_round() {
    for outcome in &OUTCOMES {
        outcome.store(0, Ordering::Relaxed);
    }
    REPORTED.store(0, Ordering::Relaxed);
    MASK.store(0, Ordering::Relaxed);
    let calls = CALL_COUNT.load(Ordering::Relaxed);
    let deadline = match CALL_DEADLINE.load(Ordering::Relaxed) {
        0 => None,
        ticks => Some(ticks),
    };
    let request = super::waitset_suite::parcel_bytes().unwrap_or_default();
    let mut sent = [0u64; CALLS];
    // The handle each call was begun with, captured here: the test replaces
    // a dead channel's entry in `CLIENTS` mid-round, and the tail must close
    // the one the call was on.
    let mut handles = [0u64; CALLS];
    for ((txn, handle), client) in sent
        .iter_mut()
        .zip(handles.iter_mut())
        .zip(&CLIENTS[..calls])
    {
        *handle = client.load(Ordering::Relaxed);
        match channels::begin_call(*handle, 1, &request, deadline) {
            Ok(begun) => *txn = begun,
            Err(_) => {
                OUTCOMES[0].store(FAILED, Ordering::Relaxed);
                STAGE.store(9, Ordering::Relaxed);
                return;
            }
        }
    }
    STAGE.store(1, Ordering::Relaxed);
    // One park on the doorbell alone: every call shares `WAIT_CALLS`.
    let mask = match channels::wait_any(&[], WAIT_CALLS, None) {
        Ok(mask) => mask,
        Err(_) => {
            OUTCOMES[0].store(FAILED, Ordering::Relaxed);
            STAGE.store(2, Ordering::Relaxed);
            return;
        }
    };
    MASK.store(mask, Ordering::Relaxed);
    if mask & CALLS_READY == 0 {
        OUTCOMES[0].store(FAILED, Ordering::Relaxed);
        STAGE.store(3, Ordering::Relaxed);
        return;
    }
    STAGE.store(4, Ordering::Relaxed);
    // Take every completion (the drain is per syscall; the caller of `take`
    // is this task).
    let mut reported: Vec<u64> = Vec::new();
    loop {
        let mut taken = [0u64; 8];
        let count = channels::completion::take(task::current(), &mut taken);
        if count == 0 {
            break;
        }
        reported.extend_from_slice(&taken[..count]);
    }
    REPORTED.store(reported.len() as u64, Ordering::Relaxed);
    STAGE.store(5, Ordering::Relaxed);
    for (index, &txn) in sent.iter().enumerate().take(calls) {
        let outcome = match channels::await_reply_now(txn) {
            Ok(_) => REPLIED,
            Err(ChannelError::Canceled) => CANCELED,
            Err(ChannelError::TimedOut) => EXPIRED,
            Err(ChannelError::PeerDied) => PEER_DIED,
            Err(ChannelError::WouldBlock) => {
                // Still pending: the round ends it cancel-first, the way a
                // completion-driven loop drops a request it gives up on.
                let _ = channels::cancel(txn);
                match channels::await_reply_now(txn) {
                    Ok(_) => REPLIED,
                    Err(ChannelError::Canceled) => CANCELED,
                    Err(error) => code_of(error),
                }
            }
            Err(error) => code_of(error),
        };
        OUTCOMES[index].store(outcome, Ordering::Relaxed);
    }
    // A peer-dead call's channel has nothing left to say: the client closes
    // its own end, so the registry slot comes back (the soak reuses slots).
    for (index, handle) in handles.iter().enumerate().take(calls) {
        if OUTCOMES[index].load(Ordering::Relaxed) == PEER_DIED {
            let _ = channels::close_endpoint(*handle);
        }
    }
    // The cancel tails put completions back: take them, so the round ends
    // with an empty queue (a next round's drain must not see them).
    loop {
        let mut taken = [0u64; 8];
        if channels::completion::take(task::current(), &mut taken) == 0 {
            break;
        }
    }
    STAGE.store(6, Ordering::Relaxed);
    ROUNDS.fetch_add(1, Ordering::Relaxed);
}

extern "C" fn client() -> ! {
    let me = task::current();
    loop {
        GATE.wait(me, None);
        client_round();
    }
}

struct Rig {
    thread: usize,
    servers: Vec<u64>,
}

fn share(thread: usize, handle: u64) -> Result<u64, String> {
    let entry = crate::ipc::handles::get(handle).map_err(|e| format!("{e:?}"))?;
    crate::ipc::handles::open_for_task(thread, entry.kind, entry.rights, entry.object_id)
        .map_err(|e| format!("{e:?}"))
}

fn rig() -> Result<Rig, String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    channels::reset();
    let thread = task::kthread::spawn_kernel_thread("completion", client, PriorityClass::Realtime)
        .map_err(|e| format!("spawn: {e}"))?;
    task::switch::yield_now();
    check!(GATE.contains(thread), "the thread did not park at its gate");
    let mut servers = Vec::new();
    for client in &CLIENTS {
        let (mine, theirs) = channels::create().map_err(|e| format!("{e:?}"))?;
        client.store(share(thread, theirs)?, Ordering::Relaxed);
        servers.push(mine);
    }
    CALL_COUNT.store(1, Ordering::Relaxed);
    CALL_DEADLINE.store(0, Ordering::Relaxed);
    ROUNDS.store(0, Ordering::Relaxed);
    Ok(Rig { thread, servers })
}

impl Rig {
    /// Send the client to run one round: it begins its calls and parks in
    /// `wait_any` mid-round. Returns the round count before it left.
    fn begin(&self) -> u64 {
        let before = ROUNDS.load(Ordering::Relaxed);
        GATE.notify_one();
        task::preempt_point();
        before
    }

    /// Wait for the round to finish — its wake, or a deadline running out
    /// (the sleeps pass the ticks) — bounded so a wedged round fails the
    /// test instead of hanging the suite.
    fn finish(&self, before: u64, label: &str) -> Result<(), String> {
        let started = task::ticks();
        while ROUNDS.load(Ordering::Relaxed) == before {
            task::wait_sleep(task::ticks() + 1);
            check!(
                task::ticks() < started + 500,
                "{label}: the round never finished (stage {}, mask {:#x}, outcome0 {}, reported {})",
                STAGE.load(Ordering::Relaxed),
                MASK.load(Ordering::Relaxed),
                OUTCOMES[0].load(Ordering::Relaxed),
                REPORTED.load(Ordering::Relaxed),
            );
        }
        Ok(())
    }

    /// `begin` then `finish`: a round nothing acts on.
    fn round(&self, label: &str) -> Result<(), String> {
        let before = self.begin();
        self.finish(before, label)
    }

    /// The request on server `index`: its transaction id.
    fn take_request(&self, index: usize) -> Result<u64, String> {
        match channels::try_recv(self.servers[index]) {
            Ok(Some(message)) => message.txn.ok_or_else(|| "a one-way request".into()),
            other => Err(format!("server {index}: no request ({other:?})")),
        }
    }

    /// The round's outcome per call, from the client's own record.
    fn outcome(&self, index: usize) -> u64 {
        OUTCOMES[index].load(Ordering::Relaxed)
    }

    /// How many completions the client's drains reported.
    fn reported(&self) -> u64 {
        REPORTED.load(Ordering::Relaxed)
    }

    /// No round may leave a registration, queue entry or transaction behind.
    fn clean(&self, label: &str) -> Result<(), String> {
        check!(
            chan::total_waiters() == 0
                && chan::queued_waiters() == 0
                && chan::live_transactions() == 0
                && channels::completion::len(self.thread) == 0,
            "{}: {} registrations, {} queue entries, {} transactions, {} completions left",
            label,
            chan::total_waiters(),
            chan::queued_waiters(),
            chan::live_transactions(),
            channels::completion::len(self.thread)
        );
        Ok(())
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

/// A reply ends the call: the doorbell reports it, one drain produces the
/// transaction, `await_reply_now` answers with the reply without parking,
/// and nothing is left behind.
pub fn completes_on_reply() -> Result<(), String> {
    let rig = rig()?;
    let reply = super::waitset_suite::parcel_bytes()?;
    CALL_COUNT.store(1, Ordering::Relaxed);
    let before = rig.begin();
    let txn = rig.take_request(0)?;
    channels::reply(txn, &reply).map_err(|e| format!("{e:?}"))?;
    rig.finish(before, "reply")?;
    check!(rig.outcome(0) == REPLIED, "outcome {}", rig.outcome(0));
    check!(
        rig.reported() == 1,
        "the reply produced {} completions",
        rig.reported()
    );
    rig.clean("reply")?;
    rig.teardown()
}

/// An unanswered call's own deadline ends it while the client is parked on
/// the doorbell alone: `EXPIRED` through the queue, and nothing left behind.
pub fn deadlines_and_cancels() -> Result<(), String> {
    let rig = rig()?;
    CALL_COUNT.store(1, Ordering::Relaxed);
    CALL_DEADLINE.store(task::ticks() + 3, Ordering::Relaxed);
    rig.round("deadline")?;
    check!(rig.outcome(0) == EXPIRED, "outcome {}", rig.outcome(0));
    check!(
        rig.reported() == 1,
        "the deadline produced {} completions",
        rig.reported()
    );
    rig.clean("deadline")?;
    CALL_DEADLINE.store(0, Ordering::Relaxed);
    rig.teardown()
}

/// Peer death ends the call while the client is parked on the doorbell:
/// `PeerDied` through the queue, and the round leaves nothing behind.
pub fn peer_death() -> Result<(), String> {
    let rig = rig()?;
    CALL_COUNT.store(1, Ordering::Relaxed);
    let before = rig.begin();
    let _ = rig.take_request(0);
    channels::close_endpoint(rig.servers[0]).map_err(|e| format!("{e:?}"))?;
    rig.finish(before, "peer death")?;
    check!(rig.outcome(0) == PEER_DIED, "outcome {}", rig.outcome(0));
    rig.clean("peer death")?;
    rig.teardown()
}

/// Soak: 6 000 rounds of two calls, ended one of three ways: the server
/// answers the first and the client cancels the one it finds still open
/// (the reply's hand-off runs it between the two), both deadlines run out,
/// or the server dies beneath the second call and the client cancels the
/// first. Every completion is reported, nothing is left behind, and a fresh
/// channel takes the dead one's slot each time.
pub fn soak() -> Result<(), String> {
    const ROUNDS_TOTAL: u64 = 6_000;
    let mut rig = rig()?;
    CALL_COUNT.store(CALLS, Ordering::Relaxed);
    for round in 0..ROUNDS_TOTAL {
        // A round that never touched a server leaves its request queued;
        // drain the stale ones so `take_request` always serves this round's
        // own (a request is the only thing ever queued on these ends).
        for server in &rig.servers {
            while channels::try_recv(*server).is_ok_and(|taken| taken.is_some()) {}
        }
        let pick = ((round * 2_654_435_761) >> 7) as usize % 3;
        CALL_DEADLINE.store(
            if pick == 1 { task::ticks() + 3 } else { 0 },
            Ordering::Relaxed,
        );
        let before = rig.begin();
        if pick == 0 {
            // Answer the first request only: the reply's hand-off runs the
            // client there (P6.2), and it cancels the call it finds still
            // open. The second request is never answered.
            let reply = super::waitset_suite::parcel_bytes()?;
            let txn0 = rig.take_request(0)?;
            channels::reply(txn0, &reply)
                .map_err(|e| format!("reply r{round}/p{pick} txn {txn0:#x}: {e:?}"))?;
        } else if pick == 2 {
            let _ = rig.take_request(0);
            let _ = rig.take_request(1);
            channels::close_endpoint(rig.servers[1]).map_err(|e| format!("{e:?}"))?;
            // A fresh channel takes the dead one's place; the client closes
            // its own end in its round tail, and this task's second handle
            // has no holder left either.
            let (mine, theirs) = channels::create().map_err(|e| format!("{e:?}"))?;
            let given = share(rig.thread, theirs)?;
            crate::ipc::handles::close(theirs).map_err(|e| format!("{e:?}"))?;
            CLIENTS[1].store(given, Ordering::Relaxed);
            rig.servers[1] = mine;
        }
        rig.finish(before, "soak")?;
        CALL_DEADLINE.store(0, Ordering::Relaxed);
        check!(
            rig.reported() >= 1,
            "round {round}: no completion was reported"
        );
        match pick {
            0 => {
                check!(
                    rig.outcome(0) == REPLIED && rig.outcome(1) == CANCELED,
                    "round {round}: reply outcomes {} / {}",
                    rig.outcome(0),
                    rig.outcome(1)
                );
            }
            1 => {
                check!(
                    rig.outcome(0) == EXPIRED && rig.outcome(1) == EXPIRED,
                    "round {round}: deadline outcomes {} / {}",
                    rig.outcome(0),
                    rig.outcome(1)
                );
            }
            _ => {
                check!(
                    rig.outcome(0) == CANCELED && rig.outcome(1) == PEER_DIED,
                    "round {round}: peer-death outcomes {} / {}",
                    rig.outcome(0),
                    rig.outcome(1)
                );
            }
        }
        rig.clean("soak")?;
    }
    crate::trace::test!("ipc_completion_soak:INFO:rounds={ROUNDS_TOTAL} calls={CALLS}");
    rig.teardown()
}

/// Another task's check is refused without observing the outcome; the
/// transaction ids are a global counter, so a foreign id is guessable and
/// only the caller's task may consume a reply (issue #725 review). The
/// call stays the client's own, and its teardown clears it.
pub fn foreign_check_refused() -> Result<(), String> {
    let rig = rig()?;
    CALL_COUNT.store(1, Ordering::Relaxed);
    rig.begin();
    let txn = rig.take_request(0)?;
    match channels::await_reply_now(txn) {
        Err(ChannelError::NotCaller) => {}
        other => check!(false, "a foreign check gave {other:?}"),
    }
    // The parking form refuses the same way, before any park.
    match channels::await_reply(txn) {
        Err(ChannelError::NotCaller) => {}
        other => check!(false, "a foreign await gave {other:?}"),
    }
    check!(
        channels::await_reply_now(txn).is_err(),
        "the foreign check must not consume the call"
    );
    rig.teardown()
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("ipc_completion_reply", completes_on_reply),
    ("ipc_completion_deadlines", deadlines_and_cancels),
    ("ipc_completion_peer_death", peer_death),
    ("ipc_completion_foreign_check", foreign_check_refused),
    ("ipc_completion_soak", soak),
];
