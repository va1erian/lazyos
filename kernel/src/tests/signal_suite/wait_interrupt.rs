//! `signal::wait_interrupted`: when a wait woken with `Interrupted` (a wait
//! set's `wait_any`) must return to the syscall boundary instead of parking
//! again. A Linux task's `SIGTERM` used to be kept, with the `SIGKILL` after
//! it, until the wait's own deadline: an idle desktop client parked for 10 s
//! ignored an orderly shutdown's stop request.

use super::*;

/// A child of the kernel task of `kind`, its frame a park inside a syscall.
fn parked_child(kind: task::Kind) -> Result<usize, String> {
    let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    task::harness::set_kind(slot, kind);
    task::harness::set_kernel_frame(slot).ok_or("the child has no frame")?;
    Ok(slot)
}

/// Collect every corpse and reset the harnesses.
fn cleanup(slots: &[usize]) -> Result<(), String> {
    for &slot in slots {
        task::harness::finish(slot, 0);
    }
    let mut reaped = 0;
    while task::reap_child().is_some() {
        reaped += 1;
    }
    check!(
        reaped == slots.len(),
        "reaped {reaped} of {} children",
        slots.len()
    );
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// A Linux task's deliverable signal ends the wait; a blocked one does not;
/// a kill always does; a native task only for a default-fatal signal.
pub fn wait_interrupted_by_linux_signals() -> Result<(), String> {
    fresh()?;
    let linux = parked_child(task::Kind::Linux)?;
    check!(
        !signal::wait_interrupted(linux),
        "a Linux task with nothing pending would leave its wait"
    );
    signal::set_blocked(linux, 1 << signal::SIGTERM);
    send(linux, signal::SIGTERM)?;
    check!(
        !signal::wait_interrupted(linux),
        "a blocked SIGTERM ended a Linux task's wait"
    );
    signal::set_blocked(linux, 0);
    check!(
        signal::wait_interrupted(linux),
        "an unblocked SIGTERM did not end a Linux task's wait"
    );
    let killed = parked_child(task::Kind::Linux)?;
    signal::set_blocked(killed, u64::MAX);
    send(killed, signal::SIGKILL)?;
    check!(
        signal::wait_interrupted(killed),
        "SIGKILL did not end a Linux task's wait"
    );
    let native = parked_child(task::Kind::Native)?;
    send(native, signal::SIGTERM)?;
    check!(
        signal::wait_interrupted(native),
        "a default-fatal SIGTERM did not end a native task's wait"
    );
    cleanup(&[linux, killed, native])
}

/// Soak: many generations of a Linux task told to stop while parked; each
/// one's wait ends, and nothing is left in the signal registry.
pub fn soak_wait_interrupted() -> Result<(), String> {
    fresh()?;
    let registry = signal::harness::registry_len();
    for round in 0..500u32 {
        let slot = parked_child(task::Kind::Linux).map_err(|e| format!("round {round}: {e}"))?;
        check!(
            !signal::wait_interrupted(slot),
            "round {round}: a fresh task would leave its wait"
        );
        send(slot, signal::SIGTERM)?;
        check!(
            signal::wait_interrupted(slot),
            "round {round}: SIGTERM did not end the wait"
        );
        task::harness::finish(slot, 0);
        check!(
            task::reap_child().is_some(),
            "round {round}: the child was not reaped"
        );
    }
    task::harness::reset();
    signal::harness::reset();
    let left = signal::harness::registry_len();
    check!(
        left <= registry,
        "signal-registry entries leaked: {registry} -> {left}"
    );
    Ok(())
}
