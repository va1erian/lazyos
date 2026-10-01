//! The native `kill` syscall (29): the supervisor's way to end one task it
//! started. Split out of `hardening.rs`.

use super::*;
use crate::ipc::credentials::{self, Cred};

/// An unprivileged user: no capabilities.
fn alice() -> Cred {
    Cred::new(1000, 1000, 0, 0, 7)
}

fn reset_creds() {
    for slot in 0..task::MAX_TASKS {
        credentials::reset_for_task(slot);
    }
}

/// The native `kill` syscall (29): one target, probe/TERM/KILL only, the same
/// uid-or-`CAP_KILL` rule as `kill(2)`, and no group or broadcast form.
pub fn native_kill_syscall_rules() -> Result<(), String> {
    use crate::process::killsys;
    const EPERM: u64 = 1u64.wrapping_neg();
    const ESRCH: u64 = 3u64.wrapping_neg();
    const EINVAL: u64 = 22u64.wrapping_neg();
    fresh()?;
    reset_creds();
    let supervisor = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let app = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let bystander = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let service = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let peer = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    // The supervisor is root; the app runs as the session user.
    credentials::set(app, alice());
    credentials::set(bystander, Cred::new(1001, 1001, 0, 0, 8));
    task::harness::switch_current(supervisor);

    check!(
        killsys::dispatch(app as u64, 0) == 0,
        "the existence probe was refused"
    );
    check!(
        task::harness::state(app) == Some(TaskState::Runnable),
        "a probe changed the target"
    );
    // Malformed requests: no group or broadcast forms, only 0/TERM/KILL.
    for (pid, sig) in [
        (0u64, signal::SIGKILL as u64),
        (u64::MAX, signal::SIGKILL as u64),
        (1u64 << 63, signal::SIGKILL as u64),
        (app as u64, 1),
        (app as u64, 11),
        (app as u64, signal::SIGSTOP as u64),
        (app as u64, 64),
        (app as u64, u64::MAX),
    ] {
        check!(
            killsys::dispatch(pid, sig) == EINVAL,
            "kill({pid:#x}, {sig}) was not EINVAL"
        );
    }
    check!(
        task::harness::state(app) == Some(TaskState::Runnable),
        "a refused request still hit the app"
    );
    check!(
        killsys::dispatch((task::MAX_TASKS - 1) as u64, signal::SIGKILL as u64) == ESRCH,
        "an empty slot was not ESRCH"
    );
    check!(
        killsys::dispatch(task::KERNEL_TASK as u64, signal::SIGKILL as u64) == EINVAL
            || killsys::dispatch(task::KERNEL_TASK as u64, signal::SIGKILL as u64) == EPERM,
        "the kernel task could be signalled"
    );
    check!(
        killsys::dispatch(app as u64, signal::SIGKILL as u64) == 0,
        "root was refused a kill of the app"
    );
    check!(
        task::harness::state(app) == Some(TaskState::Done),
        "SIGKILL did not end the app: {:?}",
        task::harness::state(app)
    );

    // An ordinary task cannot stop a task of another uid, root or not.
    credentials::set(service, alice());
    task::harness::switch_current(service);
    check!(
        killsys::dispatch(bystander as u64, signal::SIGKILL as u64) == EPERM,
        "a user killed another uid's task"
    );
    check!(
        killsys::dispatch(supervisor as u64, signal::SIGTERM as u64) == EPERM,
        "a user signalled root"
    );
    check!(
        task::harness::state(supervisor) == Some(TaskState::Runnable)
            && task::harness::state(bystander) == Some(TaskState::Runnable),
        "a refused kill still landed"
    );
    // Same uid is fine, and SIGTERM terminates too.
    credentials::set(peer, alice());
    check!(
        killsys::dispatch(peer as u64, signal::SIGTERM as u64) == 0,
        "a same-uid SIGTERM was refused"
    );
    // SIGTERM is queued for delivery (the default action runs when the task
    // next returns to user mode); only SIGKILL ends a task at the send.
    check!(
        signal::pending(peer) != 0,
        "SIGTERM was not queued for the peer"
    );

    task::harness::switch_current(task::KERNEL_TASK);
    while task::reap_child().is_some() {}
    reset_creds();
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// Soak: many start/kill/reap generations through the native syscall leave no
/// task slot, frame or pending signal behind.
pub fn soak_native_kill() -> Result<(), String> {
    use crate::process::killsys;
    fresh()?;
    reset_creds();
    let supervisor = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let before = crate::mem::frame_stats().live();
    for round in 0..1500u32 {
        let child = task::spawn_fork().map_err(|error| format!("spawn {round}: {error}"))?;
        credentials::set(child, alice());
        task::harness::switch_current(supervisor);
        let sig = signal::SIGKILL;
        check!(
            killsys::dispatch(child as u64, sig as u64) == 0,
            "round {round}: kill refused"
        );
        check!(
            task::harness::state(child) == Some(TaskState::Done),
            "round {round}: the child survived"
        );
        task::harness::switch_current(task::KERNEL_TASK);
        while task::reap_child().is_some() {}
        // The slot is free again for the next generation.
        task::harness::switch_current(supervisor);
        let probe = killsys::dispatch(child as u64, 0);
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            probe == 3u64.wrapping_neg(),
            "round {round}: the reaped slot still answers ({probe:#x})"
        );
    }
    task::harness::switch_current(task::KERNEL_TASK);
    while task::reap_child().is_some() {}
    reset_creds();
    task::harness::reset();
    signal::harness::reset();
    let after = crate::mem::frame_stats().live();
    check!(after <= before + 8, "frames leaked: {before} -> {after}");
    Ok(())
}
