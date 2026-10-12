//! `sched_setattr` from two threads at once (review of #819: the read of the
//! old state, the privilege decision and the stores were separate lock
//! sections, so two threads of one user could each pass "cannot lower nice /
//! leave IDLE / clear reset_on_fork" against a state the other was replacing).
//!
//! Two unprivileged kernel threads of one process hammer one target with
//! random requests while the test thread (the administrator) keeps resetting
//! it. Every step is checked against the rules from the state just before the
//! call and against what the thread has already seen: the stored nice value
//! never goes down, an `IDLE` task never leaves it, `reset_on_fork` never
//! clears, and the weight the scheduler holds is always the stored state's.
//!
//! The kernel threads run with interrupts off and hand over with `yield_now`
//! (the way `execmem_soak` does), so the switches are at known points; the
//! exactness of the per-call model relies on that, the monotonic and weight
//! checks do not.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use super::schedattr::*;
use super::*;
use crate::ipc::credentials;
use crate::task::linuxstate::SchedState;
use crate::task::nice;
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, WaitKind};

/// Rounds of "reset, let both threads run"; each round is `OPS_PER_ROUND`
/// calls in all.
const ROUNDS: u64 = 600;
const OPS_PER_ROUND: u64 = 24;
/// Calls a thread makes before it hands over.
const OPS_PER_TURN: u32 = 4;

static VICTIM: AtomicUsize = AtomicUsize::new(0);
/// Bumped by the test thread before it resets the target: the threads' idea
/// of what they have seen is from an older epoch and starts over.
static EPOCH: AtomicU64 = AtomicU64::new(0);
static OPS: AtomicU64 = AtomicU64::new(0);
static ACCEPTED: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);
static STOP: AtomicBool = AtomicBool::new(false);
/// The first violation any thread saw.
static FAILURE: Mutex<Option<String>> = Mutex::new(None);
static PARK: WaitQueue = WaitQueue::new(WaitKind::Sleep);

fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn fail(message: String) {
    let mut slot = FAILURE.lock();
    if slot.is_none() {
        *slot = Some(message);
    }
}

/// The weight a state implies for a task of the victim's class.
fn weight_for(state: SchedState) -> u16 {
    let class = task::priority(VICTIM.load(Ordering::SeqCst)).unwrap_or(PriorityClass::Normal);
    nice::weight(
        class.default_weight(),
        i32::from(state.nice),
        state.is_idle(),
    )
}

/// What a thread has seen of the target since the last reset.
#[derive(Clone, Copy, Default)]
struct Seen {
    epoch: u64,
    nice: i8,
    idle: bool,
    reset_on_fork: bool,
}

/// Check one snapshot against the weight rule and against `seen`.
fn observe(seen: &mut Seen, state: SchedState, weight: u16) -> Result<(), String> {
    let want = weight_for(state);
    if weight != want {
        return Err(format!(
            "stored nice {} policy {} but the weight is {weight}, want {want}",
            state.nice, state.policy
        ));
    }
    let epoch = EPOCH.load(Ordering::SeqCst);
    if seen.epoch != epoch {
        *seen = Seen {
            epoch,
            nice: state.nice,
            idle: state.is_idle(),
            reset_on_fork: state.reset_on_fork,
        };
        return Ok(());
    }
    if state.nice < seen.nice
        || (seen.idle && !state.is_idle())
        || (seen.reset_on_fork && !state.reset_on_fork)
    {
        return Err(format!(
            "an unprivileged call lowered the state: nice {} idle {} reset {} after nice {} idle {} reset {}",
            state.nice,
            state.is_idle(),
            state.reset_on_fork,
            seen.nice,
            seen.idle,
            seen.reset_on_fork
        ));
    }
    *seen = Seen {
        epoch,
        nice: state.nice,
        idle: state.is_idle(),
        reset_on_fork: state.reset_on_fork,
    };
    Ok(())
}

/// One random request, checked against the model.
fn one_call(rng: &mut u64, seen: &mut Seen) -> Result<(), String> {
    let victim = VICTIM.load(Ordering::SeqCst) as u64;
    let (before, weight) = task::sched_snapshot(victim as usize).ok_or("the target vanished")?;
    observe(seen, before, weight)?;

    let policy = [OTHER, BATCH, IDLE][(next(rng) % 3) as usize];
    let level = if next(rng).is_multiple_of(2) {
        (next(rng) % 40) as i32 - 20
    } else {
        i32::from(before.nice) + (next(rng) % 7) as i32 - 3
    }
    .clamp(-20, 19);
    let flags = if next(rng).is_multiple_of(2) {
        RESET_ON_FORK
    } else {
        0
    };
    // IDLE ignores the nice value (it keeps the task's own), so it never
    // raises; the other policies must not lower it or leave IDLE.
    let allowed = (policy == IDLE || (level >= i32::from(before.nice) && !before.is_idle()))
        && (!before.reset_on_fork || flags & RESET_ON_FORK != 0);

    let got = setattr(
        victim,
        &Attr {
            flags,
            ..attr(policy, level)
        },
    );
    OPS.fetch_add(1, Ordering::SeqCst);
    let (after, weight) = task::sched_snapshot(victim as usize).ok_or("the target vanished")?;
    if allowed {
        ACCEPTED.fetch_add(1, Ordering::SeqCst);
        let want = SchedState {
            policy: policy as u8,
            reset_on_fork: flags != 0,
            nice: if policy == IDLE {
                before.nice
            } else {
                level as i8
            },
        };
        if got != 0 || after != want {
            return Err(format!(
                "an allowed call ({before:?} -> {want:?}) returned {got:#x} and left {after:?}"
            ));
        }
    } else {
        REFUSED.fetch_add(1, Ordering::SeqCst);
        if got != EPERM || after != before {
            return Err(format!(
                "a refused call ({before:?} -> policy {policy} nice {level} flags {flags}) returned {got:#x} and left {after:?}"
            ));
        }
    }
    observe(seen, after, weight)
}

extern "C" fn hammer() -> ! {
    let me = task::current();
    let mut rng = 0x5eed_0000_0000_0001u64 ^ ((me as u64) << 40);
    let mut seen = Seen {
        epoch: u64::MAX,
        ..Seen::default()
    };
    loop {
        if STOP.load(Ordering::SeqCst) {
            PARK.wait(me, None);
            continue;
        }
        for _ in 0..OPS_PER_TURN {
            if let Err(message) = one_call(&mut rng, &mut seen) {
                fail(format!("thread {me}: {message}"));
            }
        }
        task::switch::yield_now();
    }
}

extern "C" fn parked() -> ! {
    let me = task::current();
    loop {
        PARK.wait(me, None);
    }
}

/// Start `entry` as a kernel thread of the user `USER`, in the process
/// `leader` (0: its own).
fn start(name: &'static str, entry: extern "C" fn() -> !, leader: usize) -> Result<usize, String> {
    let slot = task::kthread::spawn_kernel_thread(name, entry, PriorityClass::Interactive)
        .map_err(|error| format!("spawn {name}: {error}"))?;
    credentials::set(slot, USER);
    if leader != 0 {
        task::linuxstate::with_extras_of(slot, |extras| extras.tgid = leader);
    }
    Ok(slot)
}

/// Two unprivileged threads of one process change one task's attributes
/// 14400 times between administrator resets: no call passes a check against
/// a stale state, and the weight is always the stored state's.
pub fn schedattr_two_thread_soak() -> Result<(), String> {
    scoped(ADMIN, || {
        for counter in [&EPOCH, &OPS, &ACCEPTED, &REFUSED] {
            counter.store(0, Ordering::SeqCst);
        }
        STOP.store(false, Ordering::SeqCst);
        *FAILURE.lock() = None;
        let victim = start("schedattr-target", parked, 0)?;
        VICTIM.store(victim, Ordering::SeqCst);
        let first = start("schedattr-a", hammer, victim)?;
        let second = start("schedattr-b", hammer, victim)?;

        let outcome = (|| {
            let mut rng = 0x0ad1_0000_0000_0007u64;
            for round in 0..ROUNDS {
                // The administrator puts the target in any state at all.
                EPOCH.fetch_add(1, Ordering::SeqCst);
                let policy = [OTHER, BATCH, IDLE][(next(&mut rng) % 3) as usize];
                let level = (next(&mut rng) % 40) as i32 - 20;
                let flags = if next(&mut rng).is_multiple_of(2) {
                    RESET_ON_FORK
                } else {
                    0
                };
                let got = setattr(
                    victim as u64,
                    &Attr {
                        flags,
                        ..attr(policy, level)
                    },
                );
                check!(
                    got == 0,
                    "round {round}: the administrator's reset returned {got:#x}"
                );
                let target = OPS.load(Ordering::SeqCst) + OPS_PER_ROUND;
                while OPS.load(Ordering::SeqCst) < target && FAILURE.lock().is_none() {
                    task::switch::yield_now();
                }
                if let Some(message) = FAILURE.lock().clone() {
                    return Err(format!("round {round}: {message}"));
                }
            }
            let (accepted, refused) = (
                ACCEPTED.load(Ordering::SeqCst),
                REFUSED.load(Ordering::SeqCst),
            );
            check!(
                accepted >= ROUNDS && refused >= ROUNDS,
                "the run barely exercised both outcomes: {accepted} accepted, {refused} refused"
            );
            Ok(())
        })();

        // End the threads: park them, finish and reap them.
        STOP.store(true, Ordering::SeqCst);
        task::harness::switch_current(task::KERNEL_TASK);
        for slot in [first, second, victim] {
            task::harness::finish(slot, 0);
        }
        PARK.notify_all();
        let mut reaped = 0;
        while task::reap_child().is_some() {
            reaped += 1;
        }
        outcome?;
        check!(reaped == 3, "{reaped} of 3 threads were reapable");
        Ok(())
    })
}
