//! Blocking, waking and the `wait_*` helpers.

use super::*;

/// Park the current task (skipped by the scheduler until woken).
///
/// Kept for the kernel test harness; real blocking goes through
/// [`wait::WaitQueue`], which also records a wake reason and a deadline.
#[allow(dead_code)]
pub fn set_blocked(blocked: bool) {
    if let Some(task) = TASKS.lock()[current()].as_mut() {
        if task.state == TaskState::Done {
            return;
        }
        task.state = if blocked {
            TaskState::Blocked {
                wait: WaitKind::Sleep,
                deadline: None,
            }
        } else {
            TaskState::Runnable
        };
        task.wake_reason = None;
    }
}

/// Whether the current task is parked on a wait queue.
///
/// Kept for the kernel test harness; see [`set_blocked`].
#[allow(dead_code)]
pub fn blocked() -> bool {
    TASKS.lock()[current()]
        .as_ref()
        .is_some_and(|task| matches!(task.state, TaskState::Blocked { .. }))
}

/// Wake a parked task by slot index, recording [`WakeReason::Woken`]. Returns
/// whether the task was actually parked.
///
/// Kept for the kernel test harness; wait queues use [`wake_task_with`].
#[allow(dead_code)]
pub fn wake_task(index: usize) -> bool {
    wake_task_with(index, WakeReason::Woken)
}

/// Mark task `index` blocked with a reason and an optional absolute deadline
/// (`arch::clock::monotonic_ns`), queueing the deadline on the timer queue.
///
/// Callers park the task on a queue first and call this with interrupts
/// disabled, so the timer ISR can never schedule a half-parked task.
pub(crate) fn block_task(index: usize, wait: WaitKind, deadline: Option<u64>) {
    let mut tasks = TASKS.lock();
    if let Some(task) = tasks[index].as_mut() {
        if task.state != TaskState::Done {
            task.state = TaskState::Blocked { wait, deadline };
            task.wake_reason = None;
            set_timer(index, deadline);
        }
    }
}

/// Queue (or, for `None`, cancel) `index`'s deadline. Call with `TASKS`
/// held: the queue lock nests inside it (`timerq`).
pub(super) fn set_timer(index: usize, deadline: Option<u64>) {
    let mut timers = timerq::TIMERS.lock();
    match deadline {
        Some(deadline) => timers.arm(index, deadline),
        None => {
            timers.cancel(index);
        }
    }
}

/// Move a blocked task back to `Runnable` and record why. Returns whether the
/// task was actually blocked (a task that already timed out, or is `Done`, is
/// left untouched so the scheduler never resurrects it).
pub(crate) fn wake_task_with(index: usize, reason: WakeReason) -> bool {
    let mut tasks = TASKS.lock();
    // A task that slept while its peers ran rejoins at the current virtual
    // time instead of being handed a burst of catch-up quanta (issue #58).
    let now = virtual_now(&tasks);
    if let Some(task) = tasks[index].as_mut() {
        if let TaskState::Blocked { deadline, .. } = task.state {
            if deadline.is_some() {
                timerq::TIMERS.lock().cancel(index);
            }
            task.state = TaskState::Runnable;
            task.wake_reason = Some(reason);
            task.pass = task.pass.max(now);
            let cur = CURRENT.load(Ordering::Relaxed);
            crate::perf::on_wake(index, cur);
            super::preempt::note_wake(&tasks, index, cur);
            return true;
        }
    }
    false
}

/// Consume the wake reason recorded for `index`, if any. The wait loop calls
/// this on resume; the reason is cleared so a later wait starts fresh.
pub(crate) fn take_wake_reason(index: usize) -> Option<WakeReason> {
    TASKS.lock()[index]
        .as_mut()
        .and_then(|task| task.wake_reason.take())
}

/// Park the current task until terminal input arrives.
pub fn wait_terminal() -> WakeReason {
    wait::TERMINAL.wait(current(), None)
}

/// Park the current task until terminal input or a pipe event arrives, or
/// `deadline` (`arch::clock::monotonic_ns`) passes. The queue is advisory:
/// the caller rescans its descriptors and parks again if nothing it watches
/// changed.
pub fn wait_poll_ns(deadline: Option<u64>) -> WakeReason {
    wait::POLL.wait_ns(current(), deadline)
}

/// Wake every `poll` waiter (pipe data, space, EOF, or `-EPIPE`). Pipe code and
/// the input paths call this; wakeups are advisory.
pub fn notify_poll() {
    wait::POLL.notify_all();
    // The same event for a Messenger wait set parked on a descriptor.
    crate::ipc::channels::wake_fd_watchers();
}

/// Park the current task until `deadline` (absolute PIT ticks) passes.
#[allow(dead_code)] // the suites' tick-paced sleeps
pub fn wait_sleep(deadline: u64) -> WakeReason {
    wait_sleep_ns(ticks_to_ns(deadline))
}

/// Park the current task until `deadline` (`arch::clock::monotonic_ns`)
/// passes. Call with interrupts disabled (a syscall).
pub fn wait_sleep_ns(deadline: u64) -> WakeReason {
    wait::SLEEP.wait_ns(current(), Some(deadline))
}

/// Sleep until the next interrupt, then mask interrupts again.
///
/// For syscall loops that re-check state under spin locks between sleeps.
/// The re-check must run with interrupts off, as the syscall entry left
/// them: a tick that preempted it while it held the task table would find
/// the lock taken and spin on it forever in the scheduler, with the timer
/// masked (issue #382: `logind`'s native `read_char` hung a desktop boot
/// this way). `enable_and_hlt` also closes the race between the check and
/// the sleep.
pub fn nap() {
    crate::perf::irqoff_pause();
    // An interrupt that stops this halt may run the device bottom half: a
    // napping task holds no lock (P1.2, `preempt::interrupted_quiet_context`).
    super::preempt::nap_begin();
    x86_64::instructions::interrupts::enable_and_hlt();
    x86_64::instructions::interrupts::disable();
    super::preempt::nap_end();
    crate::perf::irqoff_resume();
}

/// Call `ready` until it yields a value, [`nap`]ping between attempts, so
/// every attempt runs with interrupts masked.
pub fn poll_until<T>(mut ready: impl FnMut() -> Option<T>) -> T {
    loop {
        if let Some(value) = ready() {
            return value;
        }
        nap();
    }
}

/// Park the current task until `deadline` (absolute PIT ticks), from a context
/// with interrupts enabled: the multiplexer's between-frames idle primitive.
///
/// Unlike [`wait_sleep`] (called from syscalls that already run with
/// interrupts disabled), this disables them around the register-then-park
/// sequence itself and restores them before returning. Sleeping between
/// frames is what bounds the mux's CPU share: an `Interactive` task that is
/// only runnable one quantum in a handful cannot starve user work.
pub fn idle(deadline: u64) -> WakeReason {
    idle_ns(ticks_to_ns(deadline))
}

/// [`idle`] until `deadline` in `arch::clock::monotonic_ns`.
pub fn idle_ns(deadline: u64) -> WakeReason {
    x86_64::instructions::interrupts::disable();
    let reason = wait::SLEEP.wait_ns(current(), Some(deadline));
    x86_64::instructions::interrupts::enable();
    reason
}

/// Park the current task until one of its children becomes reapable.
pub fn wait_child_exit() -> WakeReason {
    wait::CHILD_EXIT.wait(current(), None)
}

/// Park the current task until a signal interrupts it (no deadline): the
/// `rt_sigsuspend` wait.
pub fn wait_signal() -> WakeReason {
    wait::SLEEP.wait(current(), None)
}

/// Park the current task until a task slot is freed or `deadline` passes. The
/// Linux `clone` shim sleeps here after a spawn while the table is near
/// capacity, so earlier threads get a quantum to run, exit and free their
/// slots (which notifies the queue) before the next spawn needs one.
pub fn wait_slot(deadline: u64) -> WakeReason {
    wait::SLOT.wait(current(), Some(deadline))
}
