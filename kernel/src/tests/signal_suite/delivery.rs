//! Block/unblock and pending state, killing a blocked sleeper,
//! `SIGKILL`'s uncatchability, and `SIGCHLD` on child exit.

use super::*;

/// A blocked signal stays pending and only becomes actionable when the mask
/// clears; `SIGKILL`/`SIGSTOP` can never enter the blocked set.
pub fn block_unblock_pending() -> Result<(), String> {
    fresh()?;
    let me = task::current();
    signal::set_blocked(me, 1 << signal::SIGINT);
    send(me, signal::SIGINT)?;
    check!(
        signal::pending(me) & (1 << signal::SIGINT) != 0,
        "a blocked signal was not queued: {:#x}",
        signal::pending(me)
    );
    check!(
        signal::blocked(me) & (1 << signal::SIGINT) != 0,
        "SIGINT did not stay blocked"
    );

    // Ignored signals are not queued at all (except the SIGCHLD record).
    let some = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    signal::set_action(me, signal::SIGUSR1, Disposition::Ignore)
        .map_err(|error| format!("set_action: {error:?}"))?;
    send(me, signal::SIGUSR1)?;
    check!(
        signal::pending(me) & (1 << signal::SIGUSR1) == 0,
        "an ignored signal was queued"
    );

    // The mask filter drops the uncatchable bits.
    signal::set_blocked(me, u64::MAX);
    check!(
        signal::blocked(me) & (1 << signal::SIGKILL) == 0,
        "SIGKILL entered the blocked mask"
    );
    check!(
        signal::blocked(me) & (1 << signal::SIGSTOP) == 0,
        "SIGSTOP entered the blocked mask"
    );
    signal::set_blocked(me, 0);
    check!(
        signal::pending(me) & (1 << signal::SIGINT) != 0,
        "unblocking dropped the pending signal"
    );

    task::harness::finish(some, 0);
    check!(task::reap_child().is_some(), "child was not reapable");
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// A queued term signal wakes a parked sleeper with `Interrupted`; a
/// `SIGKILL` ends the victim outright and no later wake resurrects it.
pub fn kill_wakes_blocked() -> Result<(), String> {
    fresh()?;
    let sleeper = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let queue = task::wait::WaitQueue::new(WaitKind::Sleep);
    queue.park(sleeper, None);
    send(sleeper, signal::SIGTERM)?;
    check!(
        task::harness::state(sleeper) == Some(TaskState::Runnable),
        "SIGTERM did not wake the sleeper: {:?}",
        task::harness::state(sleeper)
    );
    check!(
        task::harness::take_wake_reason(sleeper) == Some(WakeReason::Interrupted),
        "sleeper wake reason is not Interrupted"
    );
    check!(
        signal::pending(sleeper) & (1 << signal::SIGTERM) != 0,
        "SIGTERM was not left pending"
    );

    let victim = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    queue.park(victim, None);
    send(victim, signal::SIGKILL)?;
    check!(
        task::harness::state(victim) == Some(TaskState::Done),
        "SIGKILL left the victim {:?}",
        task::harness::state(victim)
    );
    check!(
        queue.notify_all() == 0,
        "notify resurrected a killed waiter"
    );
    check!(
        task::harness::state(victim) == Some(TaskState::Done),
        "SIGKILL victim was resurrected"
    );

    task::harness::finish(sleeper, 0);
    let mut reaped = 0;
    while task::reap_child().is_some() {
        reaped += 1;
    }
    check!(reaped == 2, "init reaped {reaped} corpses, expected 2");
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// `rt_sigaction` refuses `SIGKILL`/`SIGSTOP`, accepts others and reports
/// them back; `rt_sigprocmask` cannot block the uncatchable pair.
pub fn sigkill_uncatchable() -> Result<(), String> {
    fresh()?;
    let me = task::current();
    let mut action = [0u64; 4];
    action[0] = 0x40_1000; // handler
    action[2] = 0x40_2000; // restorer
    let act = action.as_mut_ptr() as u64;

    let e = process::linux::dispatch_for_test(13, signal::SIGKILL as u64, act, 0);
    check!(
        e == (-22i64) as u64,
        "rt_sigaction(SIGKILL) returned {e:#x}"
    );
    let e = process::linux::dispatch_for_test(13, signal::SIGSTOP as u64, act, 0);
    check!(
        e == (-22i64) as u64,
        "rt_sigaction(SIGSTOP) returned {e:#x}"
    );
    check!(
        signal::action(me, signal::SIGKILL) == Disposition::Default,
        "a refused SIGKILL action changed the disposition"
    );

    // A regular signal installs, and querying returns the same action.
    let e = process::linux::dispatch_for_test(13, signal::SIGTERM as u64, act, 0);
    check!(e == 0, "rt_sigaction(SIGTERM) returned {e:#x}");
    let expected = Disposition::Handler {
        handler: 0x40_1000,
        flags: 0,
        restorer: 0x40_2000,
        mask: 0,
    };
    check!(
        signal::action(me, signal::SIGTERM) == expected,
        "installed action is {:?}",
        signal::action(me, signal::SIGTERM)
    );
    let mut old = [0u64; 4];
    let e =
        process::linux::dispatch_for_test(13, signal::SIGTERM as u64, 0, old.as_mut_ptr() as u64);
    check!(e == 0, "querying SIGTERM returned {e:#x}");
    check!(
        old == [0x40_1000, 0, 0x40_2000, 0],
        "reported action is {old:?}"
    );

    // SIGKILL/SIGSTOP bits are discarded by rt_sigprocmask. The set is a
    // Linux `sigset_t`, so the bit for `sig` is `1 << (sig - 1)`.
    let mask: u64 =
        (1 << (signal::SIGKILL - 1)) | (1 << (signal::SIGSTOP - 1)) | (1 << (signal::SIGTERM - 1));
    let e = process::linux::dispatch_for_test(
        14,
        signal::SIG_BLOCK,
        core::ptr::addr_of!(mask) as u64,
        0,
    );
    check!(e == 0, "rt_sigprocmask returned {e:#x}");
    check!(
        signal::blocked(me) & (1 << signal::SIGKILL) == 0
            && signal::blocked(me) & (1 << signal::SIGSTOP) == 0,
        "rt_sigprocmask blocked an uncatchable signal: {:#x}",
        signal::blocked(me)
    );
    check!(
        signal::blocked(me) & (1 << signal::SIGTERM) != 0,
        "rt_sigprocmask did not block SIGTERM"
    );
    signal::set_blocked(me, 0);
    signal::harness::reset();
    Ok(())
}

/// A child's exit leaves `SIGCHLD` pending on its parent (even under the
/// default ignore disposition) while `wait4` still reaps it.
pub fn sigchld_on_child_exit() -> Result<(), String> {
    fresh()?;
    let root = task::spawn_fork().map_err(|error| format!("spawn root: {error}"))?;
    task::harness::switch_current(root);
    let child = task::spawn_fork().map_err(|error| format!("spawn child: {error}"))?;
    check!(
        signal::pending(root) & (1 << signal::SIGCHLD) == 0,
        "SIGCHLD was pending before any exit"
    );
    task::harness::finish(child, 7);
    check!(
        signal::pending(root) & (1 << signal::SIGCHLD) != 0,
        "child exit did not post SIGCHLD: {:#x}",
        signal::pending(root)
    );
    let (slot, status) = task::reap_child().ok_or("parent could not reap its child")?;
    check!(
        slot == child && status == 7,
        "reaped {slot}/{status}, expected {child}/7"
    );

    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(root, 0);
    check!(task::reap_child().is_some(), "root was not reapable");
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}
