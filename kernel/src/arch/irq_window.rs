//! Interrupt windows: a long syscall briefly takes interrupts, wherever it is.
//!
//! Syscalls run with interrupts off (`docs/architecture/tasks.md`), and the
//! file calls behind a package install (`write_file`/`append_file` of 1 MiB,
//! `fsync`) kept them off for 40 to 120 ms: timer ticks were lost (caught up
//! by `arch::clock`), device interrupts waited, and the i8042's 16-byte queue
//! overflowed. Long loops in the kernel therefore call [`poll_point`], which
//! opens a window (`sti; nop; cli`) once [`WINDOW_DIV`]ths of a tick have passed
//! since the last one, so pending interrupts are taken within that bound.
//!
//! What makes a window safe at any poll point, whatever locks the syscall
//! holds, is that the handlers it admits take only one lock while it is open,
//! the i8042 FIFO's, which is held solely inside `ps2::service` with
//! interrupts off and so never across a poll point (code holding it must
//! never reach one):
//!
//! * the timer only counts the tick and acknowledges the PIC
//!   ([`window_tick`]; `task::switch` routes IRQ0 here instead of to
//!   `schedule`, so no task switch, signal sweep or deadline expiry runs),
//!   and the APIC deadline timer (`arch::event_timer`) only acknowledges:
//!   its expiry waits for the next ordinary tick;
//! * IRQ1/IRQ12 only collect the i8042's bytes (`input::ps2::service`, whose
//!   FIFO lock is never held with interrupts on), leaving the decoding for the
//!   next tick outside a window;
//! * the other PIC lines only latch and mask (`dev::irq::dispatch`, lock-free
//!   by design), and never run the device bottom half: a window is not a
//!   quiet context (`task::interrupted_quiet_context`);
//! * no handler yields: `task::preempt_point` does nothing while a window is
//!   open.
//!
//! The work these defer (charging the tick, expiry, selection, decoding) runs
//! at the next ordinary tick, at the latest just after the syscall returns.
//! Scheduling latency is still the syscall's length; interrupt latency is not.

use super::percpu;
use core::sync::atomic::{AtomicBool, Ordering};

/// A window opens once `1/WINDOW_DIV` of a timer period (1 ms at 100 Hz) has
/// passed since the previous one: half the 2 ms bound, so the work between two
/// poll points can take as long again before the bound is broken.
pub const WINDOW_DIV: u64 = 10;

/// Set once the kernel has enabled interrupts: before that the PIC and the
/// IDT may not be ready, and boot-time file reads must not open a window.
/// Machine-wide (one boot); everything else in this module is per CPU.
static ARMED: AtomicBool = AtomicBool::new(false);

/// Allow windows: called right before the kernel first enables interrupts.
pub fn arm() {
    let now = rdtsc();
    let cpu = percpu::current();
    cpu.window.last.store(now, Ordering::Relaxed);
    ARMED.store(true, Ordering::Release);
}

/// Test hook: forbid windows again (the suite arms them for itself only).
#[cfg(lazyos_tests)]
pub fn disarm() {
    ARMED.store(false, Ordering::Release);
}

/// A span with interrupts off began at `now` (`irqoff`): the next window is
/// due one window period later.
pub fn restart(now: u64) {
    percpu::current().window.last.store(now, Ordering::Relaxed);
}

/// Open a window if interrupts are off and the last one is a while ago.
/// Cheap otherwise (a flags read and a TSC read); call it freely from loops
/// that can run long.
#[inline]
#[track_caller]
pub fn poll_point() {
    if !ARMED.load(Ordering::Relaxed) || x86_64::instructions::interrupts::are_enabled() {
        return;
    }
    // A chaos image opens a window early at random (`task::chaos`).
    #[cfg(lazyos_tests)]
    if super::irqoff::span_open() && crate::task::chaos::strike() {
        open();
        return;
    }
    let due = super::clock::cycles_per_tick() / WINDOW_DIV;
    let now = rdtsc();
    let cpu = percpu::current();
    if now.wrapping_sub(cpu.window.last.load(Ordering::Relaxed)) < due {
        return;
    }
    if super::irqoff::span_open() {
        open();
    } else {
        // No span: an interrupt handler, or interrupts-off code nothing
        // accounts for. No window may open here, but the i8042 is still
        // drained so its queue cannot overflow (`input::ps2`).
        cpu.window.last.store(now, Ordering::Relaxed);
        crate::input::ps2::service();
    }
}

/// Open a window now (inside a syscall, interrupts off); otherwise do
/// nothing.
#[track_caller]
pub fn open() {
    if !may_open() {
        return;
    }
    super::irqoff::close();
    crate::perf::irqoff_pause();
    percpu::current().window.open.store(1, Ordering::SeqCst);
    // SAFETY: interrupts are enabled for exactly one instruction (`sti`
    // takes effect after the next one), so every pending interrupt is
    // delivered between the `nop` and the `cli`. While the flag above is
    // set, the only lock a handler that can run here takes is the i8042
    // FIFO's, never held at a poll point (module docs), so it cannot deadlock
    // against whatever the interrupted code holds, and
    // none of them switches tasks, so this stack resumes right after. No
    // `nomem`: the flag stores above and below must stay on their side.
    unsafe { core::arch::asm!("sti", "nop", "cli", options(nostack)) };
    // Re-fetch, don't reuse a `&mut` held across the window: a handler that
    // ran inside it (`window_tick`) took its own `&mut` to this area, and
    // two live ones are aliasing UB (the per-CPU rule: a reference from
    // `current()` must not be held across anything that can re-enter).
    let cpu = percpu::current();
    cpu.window.open.store(0, Ordering::SeqCst);
    cpu.window.opened.fetch_add(1, Ordering::Relaxed);
    cpu.window.last.store(rdtsc(), Ordering::Relaxed);
    crate::perf::irqoff_resume();
    // Restarts the span, and `LAST` again if a syscall is being charged.
    super::irqoff::resume();
}

/// Whether a window is open right now (an interrupt handler asking whether it
/// may take locks).
#[inline]
pub fn is_open() -> bool {
    percpu::current().window.open.load(Ordering::Relaxed) != 0
}

/// IRQ0 inside a window, called by `timer_isr` instead of `schedule`. Keeps
/// the clock exact and the i8042 drained; everything that needs a lock waits
/// for the next ordinary tick ([`take_uncharged`]).
#[no_mangle]
extern "C" fn window_tick() {
    if super::timer::stale_tick() {
        // An APIC tick latched before the tick was masked: acknowledged
        // there, and no tick for the kernel (as in `schedule`).
        return;
    }
    let cpu = percpu::current();
    // Only the boot CPU's tick is the clock (S8.3c, `task::schedule`); an
    // AP's is one period of its own accounting.
    let periods = if cpu.index() == 0 {
        let periods = super::clock::periods_since_last();
        super::idt::TICKS.fetch_add(periods, Ordering::Relaxed);
        periods
    } else {
        1
    };
    cpu.window
        .window_ticks
        .fetch_add(periods, Ordering::Relaxed);
    cpu.window.uncharged.fetch_add(periods, Ordering::Relaxed);
    // SAFETY: called only from `timer_isr`, i.e. in the tick handler (PIT
    // IRQ0 or the local APIC timer) with the tick in service, once.
    unsafe { super::timer::end_of_tick() };
    if cpu.index() == 0 {
        crate::input::ps2::service();
    }
}

/// Test hook: `ticks` more ticks were taken inside windows and are not yet
/// charged (the scheduler charges them at its next entry).
#[cfg(lazyos_tests)]
pub fn add_uncharged_for_test(ticks: u64) {
    percpu::current()
        .window
        .uncharged
        .fetch_add(ticks, Ordering::Relaxed);
}

/// Ticks taken inside windows since the previous call, for the scheduler to
/// charge to the task that was running them.
pub fn take_uncharged() -> u64 {
    let cpu = percpu::current();
    if cpu.window.uncharged.load(Ordering::Relaxed) == 0 {
        return 0;
    }
    cpu.window.uncharged.swap(0, Ordering::Relaxed)
}

/// Windows opened since boot.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn opened() -> u64 {
    percpu::current().window.opened.load(Ordering::Relaxed)
}

/// Timer ticks taken inside windows since boot.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn window_ticks() -> u64 {
    percpu::current()
        .window
        .window_ticks
        .load(Ordering::Relaxed)
}

/// Windows open only inside a syscall or a kernel section (an `irqoff` span
/// is being charged) with interrupts off. Never in an interrupt handler: the timer
/// acknowledges IRQ0 early, so a window there could nest ticks. A handler
/// entered from user mode, a `nap` or the kernel task finds no open span.
fn may_open() -> bool {
    ARMED.load(Ordering::Relaxed)
        && super::irqoff::span_open()
        && !x86_64::instructions::interrupts::are_enabled()
}

fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` reads a CPU counter; no memory or privilege effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}
