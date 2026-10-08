//! Interrupt windows (`arch::irq_window`) and the per-syscall interrupts-off
//! accounting (`arch::irqoff`).
//!
//! Syscalls run with interrupts off; long ones now take pending interrupts at
//! poll points, with handlers that take no lock the interrupted code may hold
//! while a window is open (only the i8042 FIFO's, never held there). These
//! tests drive the mechanism as a syscall would (a span open, `IF=0`, the PIT
//! line unmasked) with the real timer: windows stay shut outside a syscall,
//! ticks arrive through them on time and are counted exactly once, the
//! handlers run while the task table, console and serial locks are held, and
//! the accounting charges each stretch to its syscall and stops at a `nap`.
//! The ext2 soak under sustained large writes is `bcache_soak_irq_latency`.
//!
//! Timing assertions take the best of several attempts: under a loaded host
//! the vCPU is sometimes descheduled for milliseconds, which looks exactly
//! like a long stretch. A stretch the code really lacks a poll point for
//! shows up in every attempt.

use super::*;
use crate::arch::{clock, irq_window, irqchip, irqoff};

/// The latency bound the windows keep (`irqoff::REPORT_US`).
pub(in crate::tests) const BOUND_US: u64 = irqoff::REPORT_US;
/// Native syscall numbers the suite charges its fake syscalls to (unused by
/// the gate).
const NR_A: u64 = 60;
const NR_B: u64 = 61;
/// Attempts a timing assertion gets (module docs).
pub(in crate::tests) const ATTEMPTS: usize = 6;

pub(super) const CASES: &[(&str, Test)] = &[
    ("irqwin_closed_outside_syscall", closed_outside_syscall),
    ("irqwin_ticks_arrive_in_syscall", ticks_arrive_in_syscall),
    ("irqwin_handlers_take_no_lock", handlers_take_no_lock),
    (
        "irqoff_charges_spans_per_syscall",
        charges_spans_per_syscall,
    ),
    ("irqoff_nap_ends_span", nap_ends_span),
    (
        "irqoff_paused_reopens_only_open_spans",
        paused_reopens_only_open_spans,
    ),
    (
        "irqwin_soak_ticks_through_windows",
        soak_ticks_through_windows,
    ),
    (
        "irqwin_serial_drain_takes_windows",
        serial_drain_takes_windows,
    ),
    (
        "irqwin_soak_serial_drains_stay_bounded",
        soak_serial_drains_stay_bounded,
    ),
    (
        "irqwin_deadline_timer_defers_in_window",
        deadline_timer_defers_in_window,
    ),
];

/// One long serial line (`IRQWIN:SERIAL:` and padding), written as a syscall
/// would write it.
fn long_line(bytes: usize) -> String {
    let mut line = String::from("IRQWIN:SERIAL:");
    while line.len() < bytes {
        line.push('.');
    }
    line.push('\n');
    line
}

/// A kernel line drains whole with interrupts off (`serial::_write_str`);
/// at the UART's baud rate a 2 KiB line takes far longer than the bound, so
/// the drain must take interrupts at poll points (issue #400). Its worst
/// stretch stays bounded, and a drain that took long opened windows.
pub fn serial_drain_takes_windows() -> Result<(), String> {
    calibrated()?;
    kernel_task_only();
    let line = long_line(2048);
    let latency = best_of(
        "a 2 KiB serial line",
        || {
            let start = tsc();
            let ((), latency) = in_syscall(NR_A, || crate::serial::_write_str(&line));
            let took = elapsed_us(start);
            check!(
                took < 2 * BOUND_US || latency.opened >= 1,
                "a {took} µs drain opened no window ({latency:?})"
            );
            Ok(latency)
        },
        |l| l.worst_us < BOUND_US && l.missed == 0,
    )?;
    check!(
        !crate::serial::locked(),
        "the port stayed locked ({latency:?})"
    );
    Ok(())
}

/// Soak: 32 long lines back to back inside one syscall. Every tick that
/// came due arrived through a window (none missed) and the worst stretch
/// stays bounded throughout.
pub fn soak_serial_drains_stay_bounded() -> Result<(), String> {
    calibrated()?;
    kernel_task_only();
    let line = long_line(2048);
    best_of(
        "32 serial lines",
        || {
            let ((), latency) = in_syscall(NR_A, || {
                for _ in 0..32 {
                    crate::serial::_write_str(&line);
                }
            });
            Ok(latency)
        },
        |l| l.worst_us < BOUND_US && l.missed == 0 && l.window_ticks == l.ticks,
    )?;
    Ok(())
}

/// What one fake syscall saw.
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::tests) struct Latency {
    /// Worst interrupts-off stretch charged to the syscall (µs).
    pub worst_us: u64,
    /// Timer periods caught up rather than taken.
    pub missed: u64,
    /// Ticks taken inside windows, and ticks counted in all.
    pub window_ticks: u64,
    pub ticks: u64,
    /// Windows opened.
    pub opened: u64,
}

/// A lone runnable kernel task, so a real tick's `schedule` (after a `nap`)
/// has nowhere to switch to and simply resumes the test.
pub(in crate::tests) fn kernel_task_only() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// Run `f` as the body of native syscall `nr`: interrupts off, a span open,
/// windows armed and the PIT line unmasked, so poll points take real ticks.
/// The maxima are reset first; windows are disarmed again afterwards.
pub(in crate::tests) fn in_syscall<R>(nr: u64, f: impl FnOnce() -> R) -> (R, Latency) {
    let saved_mask = irqchip::is_masked(0);
    irqoff::reset();
    irq_window::arm();
    irqchip::set_masked(0, false);
    // A tick left pending by earlier tests belongs to them, not to `f`.
    clock::resync();
    let missed = clock::missed_ticks();
    let (window_ticks, ticks, opened) = (
        irq_window::window_ticks(),
        task::ticks(),
        irq_window::opened(),
    );
    irqoff::enter_native(nr);
    let result = f();
    irqoff::exit();
    irqchip::set_masked(0, saved_mask);
    irq_window::disarm();
    // No scheduler runs in the suite to charge the window ticks: drop them,
    // or the next test's scheduler entry would book them to its task.
    let _ = irq_window::take_uncharged();
    let latency = Latency {
        worst_us: irqoff::max_native_us(nr),
        missed: clock::missed_ticks() - missed,
        window_ticks: irq_window::window_ticks() - window_ticks,
        ticks: task::ticks() - ticks,
        opened: irq_window::opened() - opened,
    };
    (result, latency)
}

/// Busy-wait `us` microseconds by the TSC, calling `poll` every iteration.
pub(in crate::tests) fn spin_us(us: u64, mut poll: impl FnMut()) {
    let cycles = clock::cycles_per_tick().saturating_mul(us) / 10_000;
    let start = tsc();
    while tsc().wrapping_sub(start) < cycles {
        poll();
        core::hint::spin_loop();
    }
}

pub(in crate::tests) fn tsc() -> u64 {
    // SAFETY: `rdtsc` reads a CPU counter; no memory or privilege effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Microseconds of TSC time since `start` (a [`tsc`] reading).
pub(in crate::tests) fn elapsed_us(start: u64) -> u64 {
    irqoff::to_us(tsc().wrapping_sub(start))
}

/// The best of [`ATTEMPTS`] runs of `attempt` by `worst_us`, failing only
/// when none meets `accept` (module docs).
pub(in crate::tests) fn best_of(
    what: &str,
    mut attempt: impl FnMut() -> Result<Latency, String>,
    accept: impl Fn(&Latency) -> bool,
) -> Result<Latency, String> {
    let mut seen = Vec::new();
    for _ in 0..ATTEMPTS {
        let latency = attempt()?;
        if accept(&latency) {
            return Ok(latency);
        }
        seen.push(latency);
    }
    Err(format!("{what}: no attempt within bounds: {seen:?}"))
}

fn calibrated() -> Result<(), String> {
    check!(
        clock::cycles_per_tick() != 0,
        "the TSC is not calibrated: no time base for windows"
    );
    Ok(())
}

/// Outside a syscall (no span open) a poll point never opens a window, even
/// with a tick pending for a long time, and `IF` stays off; inside one, the
/// same pending tick is taken at the first poll point.
pub fn closed_outside_syscall() -> Result<(), String> {
    calibrated()?;
    kernel_task_only();
    irqoff::close();
    let saved_mask = irqchip::is_masked(0);
    irq_window::arm();
    irqchip::set_masked(0, false);
    let (opened, ticks) = (irq_window::opened(), task::ticks());
    spin_us(25_000, irq_window::poll_point);
    irq_window::open();
    let outside = (irq_window::opened() - opened, task::ticks() - ticks);
    let if_on = x86_64::instructions::interrupts::are_enabled();
    irq_window::disarm();
    irqchip::set_masked(0, saved_mask);
    check!(!if_on, "a poll point left interrupts on");
    check!(
        outside == (0, 0),
        "outside a syscall: {} windows opened, {} ticks taken",
        outside.0,
        outside.1
    );
    let ((), latency) = in_syscall(NR_A, || {
        spin_us(1_500, || {});
        irq_window::poll_point();
    });
    check!(
        latency.opened >= 1 && latency.window_ticks >= 1,
        "inside a syscall the pending tick was not taken: {latency:?}"
    );
    Ok(())
}

/// Polling through 20 ticks inside a syscall: every tick arrives through a
/// window, none is missed, and the syscall's worst stretch stays bounded.
pub fn ticks_arrive_in_syscall() -> Result<(), String> {
    calibrated()?;
    kernel_task_only();
    let latency = best_of(
        "20 ticks of polling",
        || {
            let (if_on, latency) = in_syscall(NR_A, || {
                let mut if_on = 0u32;
                spin_us(200_000, || {
                    irq_window::poll_point();
                    if x86_64::instructions::interrupts::are_enabled() {
                        if_on += 1;
                    }
                });
                if_on
            });
            check!(if_on == 0, "{if_on} poll points returned with IF=1");
            check!(
                task::current() == task::KERNEL_TASK,
                "a window switched tasks"
            );
            Ok(latency)
        },
        |l| l.worst_us < BOUND_US && l.missed == 0,
    )?;
    check!(
        (18..=22).contains(&latency.ticks),
        "{} ticks counted in 200 ms",
        latency.ticks
    );
    check!(
        latency.window_ticks == latency.ticks,
        "{} of {} ticks came through windows",
        latency.window_ticks,
        latency.ticks
    );
    check!(
        latency.opened >= 100,
        "only {} windows in 200 ms",
        latency.opened
    );
    Ok(())
}

/// The handlers a window admits take none of the locks the interrupted code
/// may hold (only the i8042 FIFO's, which no poll point is ever reached
/// under): windows opened while the task table, the console and the serial
/// port are all locked still take the timer (the `schedule` path, a key
/// decoded into a task's queue, or a log line would deadlock right here).
pub fn handlers_take_no_lock() -> Result<(), String> {
    calibrated()?;
    kernel_task_only();
    let (taken, latency) = in_syscall(NR_A, || {
        task::harness::with_table_locked(|| {
            crate::console::with_framebuffer(|_| {
                crate::serial::with_port_locked(|| {
                    let before = task::ticks();
                    // Long enough for at least one period to come due.
                    spin_us(25_000, irq_window::poll_point);
                    task::ticks() - before
                })
            })
        })
    });
    check!(taken.is_some(), "no console framebuffer in the test boot");
    check!(
        taken.unwrap_or(0) >= 2,
        "only {:?} ticks taken with the locks held ({latency:?})",
        taken
    );
    check!(
        !crate::task::diag::table_locked() && !crate::console::locked() && !crate::serial::locked(),
        "a lock stayed held"
    );
    Ok(())
}

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

/// A stretch without poll points is charged, whole, to its own syscall and
/// logged past the bound; a polled one of ten times the length stays under
/// it.
pub fn charges_spans_per_syscall() -> Result<(), String> {
    calibrated()?;
    kernel_task_only();
    let ((), unpolled) = in_syscall(NR_A, || spin_us(3_000, || {}));
    check!(
        unpolled.worst_us >= 2_900,
        "a 3 ms stretch was charged {} µs",
        unpolled.worst_us
    );
    check!(irqoff::over_bound() >= 1, "the stretch was not counted");
    check!(
        irqoff::max_native_us(NR_B) == 0,
        "another syscall was charged"
    );
    best_of(
        "30 ms of polling",
        || Ok(in_syscall(NR_B, || spin_us(30_000, irq_window::poll_point)).1),
        |l| l.worst_us < BOUND_US,
    )?;
    Ok(())
}

/// A `nap` (interrupts on, `hlt`) ends the stretch and the next one starts
/// when it returns: two 1.2 ms stretches around a nap of up to a tick are
/// charged as such, not as one.
pub fn nap_ends_span() -> Result<(), String> {
    calibrated()?;
    kernel_task_only();
    best_of(
        "stretches around a nap",
        || {
            let (ticks, latency) = in_syscall(NR_A, || {
                spin_us(1_200, || {});
                let before = task::ticks();
                task::nap();
                spin_us(1_200, || {});
                task::ticks() - before
            });
            check!(ticks >= 1, "the nap did not wait for a tick");
            Ok(latency)
        },
        |l| l.worst_us < 1_900,
    )?;
    Ok(())
}

/// Code that gives up the CPU or lets interrupts in (`yield_now`, the
/// `YieldMutex` halt) runs outside the span and gets it back only if it had
/// one: a switch made from an interrupt handler or an exit path must never
/// leave a span open for whatever runs next, or windows would open outside
/// a syscall.
pub fn paused_reopens_only_open_spans() -> Result<(), String> {
    calibrated()?;
    kernel_task_only();
    irqoff::close();
    let outside = irqoff::paused(irqoff::span_open);
    check!(!outside, "a span was open inside paused");
    check!(
        !irqoff::span_open(),
        "paused opened a span that was not open"
    );
    let ((inside, after), _) = in_syscall(NR_A, || {
        let inside = irqoff::paused(irqoff::span_open);
        (inside, irqoff::span_open())
    });
    check!(!inside, "the syscall's span stayed open inside paused");
    check!(after, "paused did not give the syscall its span back");
    check!(!irqoff::span_open(), "a span outlived its syscall");
    Ok(())
}

/// Soak: a second of syscall-time polling, about a thousand windows. Every
/// tick is counted exactly once (the count matches the TSC's elapsed
/// periods), and every one arrived through a window.
pub fn soak_ticks_through_windows() -> Result<(), String> {
    calibrated()?;
    kernel_task_only();
    let per_tick = clock::cycles_per_tick();
    let (elapsed, latency) = in_syscall(NR_A, || {
        let start = tsc();
        spin_us(1_000_000, irq_window::poll_point);
        tsc().wrapping_sub(start) / per_tick
    });
    check!(
        latency.ticks.abs_diff(elapsed) <= 2,
        "{} ticks counted over {elapsed} elapsed periods",
        latency.ticks
    );
    check!(
        latency.window_ticks == latency.ticks,
        "{} of {} ticks came through windows",
        latency.window_ticks,
        latency.ticks
    );
    check!(
        latency.opened >= 500,
        "only {} windows in a second",
        latency.opened
    );
    Ok(())
}
