//! The child-exit doorbell (docs/performance-plan.md P7.1).
//!
//! A supervisor has two sources of work: requests on its Messenger endpoint
//! and its children's exits. The native `wait` (syscall 7) parks on the
//! child-exit queue and `recv` on the Messenger one, so `init` used to park
//! in `wait` with a 50 ms deadline and then poll its endpoint: every request
//! waited up to 50 ms, and an idle `init` woke 20 times a second. With this
//! bell a task parks once, in `channels::wait_any` with `WAIT_CHILD`, and is
//! woken by whichever comes first.
//!
//! The bell is level-triggered: arming reports ready while the task has a
//! finished child it has not reaped, so a task that reaps one exit per wake
//! never loses the next. Arming and ringing both run with interrupts off on
//! one CPU (syscall context, or after the task table lock is dropped on the
//! exit paths), so no exit slips between the check and the park. The state is
//! one bit per task slot, set while that task is parked on the bell.

use core::sync::atomic::{AtomicU64, Ordering};

use super::{TaskState, MAX_TASKS, TASKS};

const WORDS: usize = MAX_TASKS.div_ceil(64);

/// Bit `slot` is set while `slot` is parked on the bell.
static ARMED: [AtomicU64; WORDS] = [const { AtomicU64::new(0) }; WORDS];

fn bit(slot: usize) -> Option<(&'static AtomicU64, u64)> {
    ARMED.get(slot / 64).map(|word| (word, 1u64 << (slot % 64)))
}

/// `me` arms the bell before parking. `true`: a finished child is waiting to
/// be reaped already (nothing is armed).
pub fn arm(me: usize) -> bool {
    if has_finished_child(me) {
        return true;
    }
    if let Some((word, mask)) = bit(me) {
        word.fetch_or(mask, Ordering::AcqRel);
    }
    false
}

/// Withdraw `me`'s registration, if it is still armed.
pub fn disarm(me: usize) {
    if let Some((word, mask)) = bit(me) {
        word.fetch_and(!mask, Ordering::AcqRel);
    }
}

/// A child of `parent` finished: wake `parent` if it is parked on the bell.
/// Takes the Messenger queue and task-table locks: call with neither held
/// (the exit paths call it next to their `CHILD_EXIT` notification).
pub fn ring(parent: usize) {
    let Some((word, mask)) = bit(parent) else {
        return;
    };
    if word.fetch_and(!mask, Ordering::AcqRel) & mask != 0 {
        crate::ipc::channels::wake_parked(parent);
    }
}

/// Whether `slot` is parked on the bell (tests).
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn armed(slot: usize) -> bool {
    bit(slot).is_some_and(|(word, mask)| word.load(Ordering::Acquire) & mask != 0)
}

/// Whether `me` has a finished child it has not reaped yet.
fn has_finished_child(me: usize) -> bool {
    TASKS.lock().iter().enumerate().any(|(slot, task)| {
        slot != me
            && task
                .as_ref()
                .is_some_and(|task| task.parent == me && task.state == TaskState::Done)
    })
}
