//! The APIC deadline timer inside an interrupt window (`arch::event_timer`,
//! `arch::irq_window`): a regression test of the deadlock it caused, beside
//! the window suite whose helpers it uses.

use super::irq_window_suite::{calibrated, in_syscall, kernel_task_only, spin_us, NR_A};
use super::*;
use crate::arch::{clock, irq_window};

/// The APIC deadline timer firing inside a window takes no lock: with the
/// task table held (as the exit path holds it while it prints, and the
/// serial drain opens windows), a deadline that comes due is acknowledged and
/// left to the next ordinary tick. Its expiry, which takes the table, used to
/// run there and spin forever with interrupts off. A waiter whose deadline
/// passed meanwhile is still blocked when the window ends (nothing expired
/// it there), and the expiry the next tick runs (`task::expire_due`, here
/// called directly: a real tick would switch to the woken task) times it
/// out. That a tick reaches that expiry is the deadline suite's.
pub fn deadline_timer_defers_in_window() -> Result<(), String> {
    calibrated()?;
    if !crate::arch::event_timer::available() {
        return Ok(()); // No deadline timer on this machine: nothing to defer.
    }
    kernel_task_only();
    for _ in 0..20 {
        let (taken, _) = in_syscall(NR_A, || {
            task::harness::with_table_locked(|| {
                let now = clock::monotonic_ns();
                crate::arch::event_timer::program(Some(now + 2_000_000), now);
                let before = task::ticks();
                spin_us(25_000, irq_window::poll_point);
                task::ticks() - before
            })
        });
        check!(taken >= 2, "only {taken} ticks taken with a deadline armed");
    }
    check!(
        !crate::task::diag::table_locked(),
        "the task table stayed locked"
    );
    // Deferred, not lost: a waiter whose deadline passed inside the window
    // is still blocked afterwards, then timed out by the ordinary expiry.
    let waiter = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
    let queue = crate::task::wait::WaitQueue::new(crate::task::WaitKind::Sleep);
    let deadline = clock::monotonic_ns() + 2_000_000;
    queue.park_ns(waiter, Some(deadline));
    let ((), _) = in_syscall(NR_A, || {
        task::harness::with_table_locked(|| {
            crate::arch::event_timer::program(Some(deadline), clock::monotonic_ns());
            spin_us(25_000, irq_window::poll_point);
        })
    });
    check!(
        matches!(
            task::harness::state(waiter),
            Some(crate::task::TaskState::Blocked { .. })
        ),
        "the deadline expired inside the window: {:?}",
        task::harness::state(waiter)
    );
    task::expire_due();
    let reason = task::harness::take_wake_reason(waiter);
    check!(
        reason == Some(crate::task::WakeReason::TimedOut),
        "the deferred deadline was lost: {reason:?}"
    );
    queue.notify_all();
    task::harness::reset();
    Ok(())
}
