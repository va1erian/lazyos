//! Wait queues: park a task until an event or a deadline (issue #57).
//!
//! Every blocking Linux syscall has the same shape — "park until a condition
//! holds or a timeout expires" — so that shape lives here once:
//!
//! * a waiter registers on a [`WaitQueue`], marks its task `Blocked`, and
//!   yields the CPU through the voluntary-reschedule gate
//!   ([`super::switch::yield_now`]). The switch saves a normal interrupt
//!   frame (the same layout as a timer tick), so another task runs while the
//!   waiter sleeps and the syscall's kernel stack is restored untouched when
//!   the waiter is picked again. A park is not a tick: it neither advances
//!   the clock nor charges CPU time (issue #338);
//! * [`WaitQueue::notify_one`] / [`WaitQueue::notify_all`] move waiters back to
//!   `Runnable` synchronously and record [`super::WakeReason::Woken`]. The
//!   scheduler only ever picks `Runnable` tasks, so the wake takes effect
//!   without waiting for a timer tick;
//! * every scheduler entry (tick or park) sweeps expired deadlines (`task::expire_deadlines`) and
//!   marks those waiters `TimedOut`, so a parked task can never outlive its
//!   deadline even if nothing notifies its queue.
//!
//! Locking: a wait queue is always locked *before* the task table, never after
//! (notify takes the queue lock and then updates `TASKS`). Callers must hold no
//! locks across [`WaitQueue::wait`], and must call it with interrupts disabled
//! so the register-then-block sequence cannot be split by the timer.

use alloc::vec::Vec;
use spin::Mutex;

use super::{block_task, take_wake_reason, wake_task_with, WaitKind, WakeReason};

/// Terminal input waiters: blocking `read` and `poll` on fd 0. The keyboard
/// IRQ and injected terminal replies notify this queue. A single shared queue
/// is enough because wakeups are advisory — a reader re-checks its own input
/// buffer and parks again if the key was not for it.
pub static TERMINAL: WaitQueue = WaitQueue::new(WaitKind::Terminal);

/// Parents parked in `wait4`. `finish_current` notifies it when any task exits;
/// a parent re-checks `reap_child` and waits again if the exit was not its
/// child's.
pub static CHILD_EXIT: WaitQueue = WaitQueue::new(WaitKind::ChildExit);

/// `nanosleep` sleepers. Nothing notifies this queue: the deadline sweep wakes
/// them with `TimedOut`, which is exactly a sleep.
pub static SLEEP: WaitQueue = WaitQueue::new(WaitKind::Sleep);

/// `clone` callers parked while the task table is near capacity. Slot
/// reclamation and `reap_child` notify it so a spawner returns as soon as a
/// slot is free.
pub static SLOT: WaitQueue = WaitQueue::new(WaitKind::Slot);

/// `poll` waiters over mixed descriptors. Terminal input and every pipe event
/// notify it; wakeups are advisory, so a waiter rescans its own `pollfd` array
/// and parks again if nothing it cares about changed.
pub static POLL: WaitQueue = WaitQueue::new(WaitKind::Poll);

struct QueueState {
    /// Parked task slots, oldest first (FIFO wake order).
    waiters: Vec<usize>,
}

/// A FIFO of parked tasks keyed by the event they wait on.
pub struct WaitQueue {
    kind: WaitKind,
    state: Mutex<QueueState>,
}

impl WaitQueue {
    pub const fn new(kind: WaitKind) -> Self {
        Self {
            kind,
            state: Mutex::new(QueueState {
                waiters: Vec::new(),
            }),
        }
    }

    /// Register `task` and mark it `Blocked`, without yielding.
    ///
    /// Call with interrupts disabled (syscall context). The enqueue and the
    /// state change then look atomic to the timer ISR: no tick can select the
    /// task in between, and no notifier can miss it afterwards.
    pub(crate) fn park(&self, task: usize, deadline: Option<u64>) {
        self.state.lock().waiters.push(task);
        block_task(task, self.kind, deadline);
    }

    /// Block `task` until this queue wakes it or `deadline` (absolute PIT
    /// ticks, 100 Hz) passes; `None` means no timeout.
    ///
    /// Must be called by the task itself with interrupts disabled. Returns as
    /// soon as the wake reason is observed, which can be on the very tick that
    /// woke it (deadline sweep) rather than a tick later.
    pub fn wait(&self, task: usize, deadline: Option<u64>) -> WakeReason {
        self.park(task, deadline);
        // Enter the scheduler through the voluntary gate: the saved context is
        // a regular interrupt frame, so resuming later lands right here with
        // the blocking syscall's stack still intact.
        super::switch::yield_now();
        loop {
            if let Some(reason) = take_wake_reason(task) {
                // The deadline sweep and notifiers leave the waiter enqueued;
                // removing it here keeps queue updates on the queue lock only.
                self.state.lock().waiters.retain(|&waiter| waiter != task);
                return reason;
            }
            // Not woken: sleep until a tick (possibly the deadline sweep) or a
            // notifier makes us runnable again. `enable_and_hlt` closes the
            // race between the check above and the sleep; interrupts go off
            // again before the re-check so the caller's contract (and its
            // next register-then-park) still holds when `wait` returns.
            super::nap();
            #[cfg(lazyos_tests)]
            super::harness::run_nap_hook();
        }
    }

    /// Wake at most `count` waiters, oldest first. Returns how many tasks
    /// actually moved to `Runnable`: entries that already timed out (or never
    /// were blocked) are dropped without consuming the budget.
    pub fn notify(&self, count: usize) -> usize {
        let mut state = self.state.lock();
        let mut woken = 0;
        while woken < count {
            let Some(index) = state.waiters.first().copied() else {
                break;
            };
            state.waiters.remove(0);
            if wake_task_with(index, WakeReason::Woken) {
                woken += 1;
            }
        }
        woken
    }

    /// Wake `task` only, if it is parked on this queue (issue #338's targeted
    /// wakeups). Every entry for `task` is removed, like the wait loop does
    /// on return. Returns whether the task actually moved to `Runnable`: a
    /// task that is not enqueued here (already woken, timed out, or blocked
    /// on another queue) is left untouched.
    pub fn notify_task(&self, task: usize) -> bool {
        let mut state = self.state.lock();
        let before = state.waiters.len();
        state.waiters.retain(|&waiter| waiter != task);
        if state.waiters.len() == before {
            return false;
        }
        wake_task_with(task, WakeReason::Woken)
    }

    /// Whether `task` is enqueued here (test and diagnostics hook).
    #[allow(dead_code)]
    pub fn contains(&self, task: usize) -> bool {
        self.state.lock().waiters.contains(&task)
    }

    /// Number of queued entries, duplicates included (test hook: a leak shows
    /// up as a length that never returns to zero).
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.state.lock().waiters.len()
    }

    /// Wake a single waiter (the oldest one), if any.
    #[allow(dead_code)]
    pub fn notify_one(&self) -> usize {
        self.notify(1)
    }

    /// Wake every waiter.
    pub fn notify_all(&self) -> usize {
        self.notify(usize::MAX)
    }

    /// Whether any task is parked here.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.state.lock().waiters.is_empty()
    }
}
