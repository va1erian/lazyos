//! `WaitQueue` park/wake ordering, deadline timeouts, and the
//! scheduler skipping blocked tasks.

use super::*;

/// A wait queue parks a task and `notify_one` moves it back to `Runnable`
/// with reason `Woken`, synchronously (no timer tick is involved).
pub fn wait_queue_block_wake() -> Result<(), String> {
    task::register_kernel();
    let me = task::current();
    let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
    queue.park(me, None);
    check!(
        matches!(
            task::harness::state(me),
            Some(task::TaskState::Blocked { .. })
        ),
        "park did not block the task: {:?}",
        task::harness::state(me)
    );
    check!(queue.notify_one() == 1, "notify_one did not wake a waiter");
    check!(
        task::harness::state(me) == Some(task::TaskState::Runnable),
        "woken task is not runnable: {:?}",
        task::harness::state(me)
    );
    check!(
        task::harness::take_wake_reason(me) == Some(task::WakeReason::Woken),
        "wake reason is not Woken: {:?}",
        task::harness::take_wake_reason(me)
    );
    check!(
        queue.notify_one() == 0,
        "notify_one woke a task that was not parked"
    );
    Ok(())
}

/// The deadline sweep leaves a task parked before its deadline and wakes it
/// with `TimedOut` exactly at the deadline.
pub fn wait_queue_deadline_timeout() -> Result<(), String> {
    task::register_kernel();
    let me = task::current();
    let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
    let now = task::ticks();
    queue.park(me, Some(now + 10));
    task::harness::expire_deadlines(now + 9);
    check!(
        matches!(
            task::harness::state(me),
            Some(task::TaskState::Blocked { .. })
        ),
        "task woke before its deadline: {:?}",
        task::harness::state(me)
    );
    task::harness::expire_deadlines(now + 10);
    check!(
        task::harness::state(me) == Some(task::TaskState::Runnable),
        "deadline did not wake the task: {:?}",
        task::harness::state(me)
    );
    check!(
        task::harness::take_wake_reason(me) == Some(task::WakeReason::TimedOut),
        "deadline wake reason is not TimedOut: {:?}",
        task::harness::take_wake_reason(me)
    );
    // The sweep leaves the waiter enqueued; a later notify must drop it
    // without counting it as woken.
    check!(
        queue.notify_all() == 0,
        "a timed-out waiter was counted as woken"
    );
    Ok(())
}

/// `notify_one` wakes the oldest waiter first; `notify_all` drains the rest.
pub fn wait_queue_notify_all_order() -> Result<(), String> {
    task::harness::reset();
    let first = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let second = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let third = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
    queue.park(first, None);
    queue.park(second, None);
    queue.park(third, None);

    check!(
        queue.notify_one() == 1,
        "notify_one should wake exactly the oldest waiter"
    );
    check!(
        task::harness::state(first) == Some(task::TaskState::Runnable),
        "oldest waiter was not woken first"
    );
    check!(
        task::harness::state(second)
            == Some(task::TaskState::Blocked {
                wait: task::WaitKind::Sleep,
                deadline: None
            }),
        "second waiter woke before the first"
    );
    check!(
        task::harness::state(third)
            == Some(task::TaskState::Blocked {
                wait: task::WaitKind::Sleep,
                deadline: None
            }),
        "third waiter woke before the first"
    );

    check!(queue.notify_all() == 2, "notify_all did not drain the rest");
    check!(
        task::harness::state(second) == Some(task::TaskState::Runnable)
            && task::harness::state(third) == Some(task::TaskState::Runnable),
        "notify_all left a waiter parked"
    );

    for slot in [first, second, third] {
        task::harness::finish(slot, 0);
        check!(
            task::reap_child().is_some(),
            "child {slot} was not reapable"
        );
    }
    task::harness::reset();
    Ok(())
}

/// The scheduler selection skips blocked tasks and picks them again only
/// after a wake.
pub fn wait_queue_blocked_not_scheduled() -> Result<(), String> {
    task::harness::reset();
    let child = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
    queue.park(child, None);
    check!(
        task::harness::next_runnable() != child,
        "selection picked blocked task {child}"
    );
    check!(queue.notify_one() == 1, "notify_one did not wake the child");
    check!(
        task::harness::next_runnable() == child,
        "selection did not pick the woken task"
    );
    task::harness::finish(child, 0);
    check!(task::reap_child().is_some(), "child was not reapable");
    task::harness::reset();
    Ok(())
}
