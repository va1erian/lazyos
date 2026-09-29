//! `rt_sigsuspend`'s mask handling (the blocking park itself needs a live
//! scheduler, which the harness lacks, so these drive the signal-layer pieces
//! the syscall and `deliver_linux` use): the temporary mask replaces the
//! blocked set, a signal it lets through is deliverable, and the original mask
//! comes back exactly once.

use super::*;

/// The temporary mask makes a previously blocked signal deliverable and the
/// original mask is restored afterwards; nesting keeps the outermost mask and
/// ending with nothing outstanding changes nothing.
pub fn suspend_swaps_and_restores_the_mask() -> Result<(), String> {
    fresh()?;
    let me = task::current();
    let original = (1 << signal::SIGCHLD) | (1 << signal::SIGUSR1);
    // A real handler, so the pending signal is one `rt_sigsuspend` must wake for.
    signal::set_action(
        me,
        signal::SIGCHLD,
        Disposition::Handler {
            handler: 0x0040_0100,
            flags: 0,
            restorer: 0x0040_0200,
            mask: 0,
        },
    )
    .map_err(|error| format!("set_action: {error:?}"))?;
    signal::set_blocked(me, original);
    send(me, signal::SIGCHLD)?;
    check!(
        !signal::suspend_wake_ready(me),
        "a blocked pending signal was deliverable before the suspend"
    );

    signal::suspend_begin(me, 0);
    check!(
        signal::blocked(me) == 0,
        "the temporary mask was not installed"
    );
    check!(
        signal::suspend_wake_ready(me),
        "the signal is not deliverable under the temporary mask"
    );

    // A second suspend before the first ends keeps the outermost mask.
    signal::suspend_begin(me, 1 << signal::SIGUSR2);
    signal::suspend_end(me);
    check!(
        signal::blocked(me) == original,
        "the mask after suspend_end is {:#x}, expected {original:#x}",
        signal::blocked(me)
    );

    // Nothing outstanding: a stray end must not disturb the mask.
    signal::suspend_end(me);
    check!(
        signal::blocked(me) == original,
        "a stray suspend_end changed the mask"
    );
    Ok(())
}

/// Soak: many suspend/restore cycles leave the mask exactly as it started (a
/// leak would drift the blocked set of a long-running shell).
pub fn soak_suspend_cycles() -> Result<(), String> {
    fresh()?;
    let me = task::current();
    let original = 1 << signal::SIGINT;
    signal::set_blocked(me, original);
    for round in 0..2000u64 {
        signal::suspend_begin(me, round & 0xffff);
        signal::suspend_end(me);
        check!(
            signal::blocked(me) == original,
            "round {round}: mask drifted to {:#x}",
            signal::blocked(me)
        );
    }
    Ok(())
}

/// A pending signal whose action is "ignore" must not end the suspension (it is
/// discarded), and only the suspending task can end its own suspend.
pub fn suspend_ignores_ignored_signals_and_is_per_task() -> Result<(), String> {
    fresh()?;
    let me = task::current();
    let original = 1 << signal::SIGCHLD;
    signal::set_blocked(me, original);
    // SIGCHLD's default action is ignore; a record of it is still queued.
    send(me, signal::SIGCHLD)?;
    signal::suspend_begin(me, 0);
    check!(
        !signal::suspend_wake_ready(me),
        "an ignored pending signal woke the suspend"
    );
    check!(
        signal::pending(me) & (1 << signal::SIGCHLD) == 0,
        "the ignored signal was left pending"
    );

    // Another task of the process finishing its own syscall must not consume
    // this task's saved mask.
    signal::suspend_end_as_other(me, me + 1);
    check!(
        signal::blocked(me) == 0,
        "a sibling's syscall return ended this task's suspend"
    );
    signal::suspend_end(me);
    check!(
        signal::blocked(me) == original,
        "the owner could not end its suspend"
    );
    Ok(())
}
