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
    signal::set_blocked(me, original);
    send(me, signal::SIGCHLD)?;
    check!(
        !signal::deliverable_pending(me),
        "a blocked pending signal was deliverable before the suspend"
    );

    signal::suspend_begin(me, 0);
    check!(
        signal::blocked(me) == 0,
        "the temporary mask was not installed"
    );
    check!(
        signal::deliverable_pending(me),
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
