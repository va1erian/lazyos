//! The "handler frame deferred" mark (`signal::frame_deferred`) that makes a
//! kernel wait (`WaitFor`, `WaitQueue::wait`) return `Interrupted` instead of
//! parking: it must not outlive the signal it was set for, and the delivery
//! the wait's `EINTR` leads to must arm the handler and clear it.

use super::group::*;
use super::*;
use crate::task::signal::{self, SigInfo};

/// A mark whose signal was taken elsewhere or is blocked has nothing behind
/// it: `frame_deferred` says no and clears it, so a wait parks, and the mark
/// does not come back when the signal is unblocked later.
pub fn threadsig_stale_deferred_mark_is_cleared() -> Result<(), String> {
    let g = group(1)?;
    let a = g.threads[0];
    // Nothing pending at all (a sibling took the process-directed signal).
    signal::harness::set_frame_deferred(a, true);
    check!(
        !signal::frame_deferred(a),
        "a mark with no signal behind it"
    );
    // A signal pending in the process, but A blocks it.
    signal::set_blocked(a, kbit(USR1));
    signal::send_to_slot(
        task::KERNEL_TASK,
        g.leader,
        USR1,
        SigInfo::user(task::KERNEL_TASK, signal::SI_USER),
    )
    .map_err(|e| format!("send {e:?}"))?;
    signal::harness::set_frame_deferred(a, true);
    check!(
        !signal::frame_deferred(a),
        "a blocked signal kept the mark alive"
    );
    // Unblocking later: the mark was cleared by the look above, not revived.
    signal::set_blocked(a, 0);
    check!(
        !signal::frame_deferred(a),
        "the cleared mark came back with the signal"
    );
    teardown(&g)
}

/// A mark with a deliverable signal stays, and the scheduler resuming the
/// thread (what the std runtime's `sched_yield` after the wait's `EINTR`
/// leads to) lays out the handler frame: the thread resumes in its handler,
/// the signal is masked for its duration, and the mark is gone.
///
/// The `sched_yield` dispatch itself is not run: it switches context, and
/// the suite's `switch_current` is not a real context (the saved frame of a
/// task that never ran is what the scheduler would read). `resume_delivery`
/// is the `deliver_on_resume` the scheduler runs on that switch.
pub fn threadsig_deferred_frame_is_delivered_on_resume() -> Result<(), String> {
    let g = group(1)?;
    let a = g.threads[0];
    check!(tgkill(&g, task::KERNEL_TASK, a, USR1) == 0, "tgkill");
    signal::harness::set_frame_deferred(a, true);
    check!(
        signal::frame_deferred(a),
        "a mark with a deliverable signal was dropped"
    );
    check!(!g.entered(a), "the handler was entered before delivery");
    check!(!g.resume(a), "the delivery ended the thread");
    check!(g.entered(a), "the handler frame was not laid out");
    check!(
        signal::blocked(a) & kbit(USR1) != 0,
        "the handler runs without its signal masked"
    );
    check!(!signal::frame_deferred(a), "the mark outlived the delivery");
    teardown(&g)
}

pub(crate) const WAITFOR_CASES: &[(&str, Test)] = &[
    (
        "compat_threadsig_stale_deferred_mark_is_cleared",
        threadsig_stale_deferred_mark_is_cleared,
    ),
    (
        "compat_threadsig_deferred_frame_is_delivered_on_resume",
        threadsig_deferred_frame_is_delivered_on_resume,
    ),
];
