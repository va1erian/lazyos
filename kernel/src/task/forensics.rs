//! The freeze-forensics reports the multiplexer loop runs (issue #861).
//!
//! Each reporter looks at the task table (or its own record) a few times a
//! second at most, prints only when something is wrong, and does it from task
//! context with the table lock released: never from the timer, never under
//! `TASKS`, and through the kernel's trace API so a mask or a budget can
//! silence it. What each one says:
//!
//! * [`procexit`](super::procexit): `PROC:EXIT`, one line per process that ended;
//! * [`stuck`](super::stuck): `SCHED:STUCK`, a task runnable inside the kernel;
//! * [`cpustat`](super::cpustat): `PERF:CPU`, the busy processes;
//! * [`lockwait`](super::lockwait): `LOCK:WAIT`, a long wait for a lock;
//! * `serial::report_backlog`: `SERIAL:BACKLOG`, the transmit ring filling up.

use core::sync::atomic::{AtomicU64, Ordering};

/// Ticks (100 Hz) between two looks at the whole table: the multiplexer loop
/// runs every two ticks while it paints, and a table sweep is not for that.
const SAMPLE_TICKS: u64 = 10;

/// Run every reporter once. Call with interrupts off from the kernel task.
pub fn service() {
    static LAST: AtomicU64 = AtomicU64::new(0);
    super::procexit::report();
    let now = super::ticks();
    if now.wrapping_sub(LAST.load(Ordering::Relaxed)) < SAMPLE_TICKS {
        return;
    }
    LAST.store(now, Ordering::Relaxed);
    super::stuck::sample();
    super::cpustat::sample();
    super::lockwait::sample();
}
