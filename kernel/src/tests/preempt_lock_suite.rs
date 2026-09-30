//! Locks shared with non-preemptible code must never be held preemptibly
//! (issue #382), and the NMI hang report that diagnosed it.
//!
//! The desktop boot hung when a timer tick preempted the kernel mux in the
//! middle of a full-screen blit: the mux held the console lock with
//! interrupts on, the tick handed the CPU to `xuid`, and `xuid`'s `present`
//! syscall spun on that lock with interrupts off, forever. The heap has the
//! same shape (the mux allocates preemptibly, syscalls and the scheduler's
//! signal sweep with interrupts off). Both locks are now taken with
//! interrupts masked, so a tick can never find them held.

use super::*;
use crate::arch::nmi::{self, Interrupted};
use crate::arch::pic;
use crate::task::harness;

/// The PIT's PIC line.
const TIMER_LINE: u8 = 0;
/// Real timer ticks the soak runs through (one second at 100 Hz).
const SOAK_TICKS: u64 = 100;
/// Iteration cap, so a dead PIT fails the soak instead of hanging it.
const SOAK_MAX_ROUNDS: u64 = 50_000_000;

/// A lone runnable kernel task, so a real tick's `schedule` has nowhere to
/// switch to and simply resumes the test.
fn kernel_task_only() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// Run `f` with interrupts on and only `line` unmasked at the PIC (none when
/// `line` is `None`), restoring every mask and `IF=0` afterwards.
fn with_irqs_on(line: Option<u8>, f: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let saved: [bool; 16] = core::array::from_fn(|l| pic::is_masked(l as u8));
    for l in 0..16u8 {
        pic::set_masked(l, Some(l) != line);
    }
    x86_64::instructions::interrupts::enable();
    let result = f();
    x86_64::instructions::interrupts::disable();
    for (l, masked) in saved.into_iter().enumerate() {
        pic::set_masked(l as u8, masked);
    }
    result
}

/// The console lock is held with interrupts off even when the caller runs
/// with them on (the mux), and the caller's `IF` comes back.
pub fn console_lock_masks_interrupts() -> Result<(), String> {
    with_irqs_on(None, || {
        let inside = crate::console::with_framebuffer(|_| {
            (
                x86_64::instructions::interrupts::are_enabled(),
                crate::console::locked(),
            )
        });
        check!(inside.is_some(), "no console framebuffer in the test boot");
        check!(
            inside == Some((false, true)),
            "inside with_framebuffer (IF, locked) = {inside:?}, expected (false, true)"
        );
        check!(
            !crate::console::locked(),
            "the console lock was not released"
        );
        check!(
            x86_64::instructions::interrupts::are_enabled(),
            "with_framebuffer did not restore IF=1"
        );
        Ok(())
    })
}

/// Every heap critical section runs with interrupts off, from an `IF=1`
/// caller too, and hands `IF=1` back.
pub fn heap_lock_masks_interrupts() -> Result<(), String> {
    let _ = crate::mem::heap_harness::take();
    with_irqs_on(None, || {
        for size in 1..=512usize {
            let block = alloc::vec![0u8; size * 8];
            core::hint::black_box(&block);
        }
        check!(
            x86_64::instructions::interrupts::are_enabled(),
            "allocation did not restore IF=1"
        );
        Ok(())
    })?;
    let (sections, unmasked) = crate::mem::heap_harness::take();
    check!(sections >= 1024, "only {sections} heap sections observed");
    check!(
        unmasked == 0,
        "{unmasked} of {sections} heap sections ran with interrupts on"
    );
    check!(!crate::mem::heap_locked(), "the heap lock is still held");
    Ok(())
}

/// One round of mux-shaped work: heap churn of varied sizes, and a small
/// blit through the console lock every eighth round.
fn mux_round(round: u64) {
    let block = alloc::vec![round as u8; 16 + (round as usize * 97) % 8192];
    core::hint::black_box(&block);
    drop(block);
    if round % 8 == 0 {
        let pixels = [0x40u8; 32 * 32 * 4];
        crate::console::with_framebuffer(|fb| {
            fb.blit_rgba_region(&pixels, 32, 32, 0, 0, 0, 0, 32, 32)
        });
    }
}

/// Soak with the real PIT: a second of ticks lands on a kernel context that
/// does nothing but heap and console work with `IF=1`, as the mux does. No
/// tick may find either lock held: every such tick is a preempted holder,
/// the precondition of the #382 hang. Fails on the unmasked locks within a
/// handful of ticks.
pub fn soak_ticks_never_preempt_lock_holders() -> Result<(), String> {
    kernel_task_only();
    let _ = harness::take_tick_lock_stats();
    let start = task::ticks();
    let mut rounds = 0u64;
    with_irqs_on(Some(TIMER_LINE), || {
        while task::ticks() < start + SOAK_TICKS && rounds < SOAK_MAX_ROUNDS {
            mux_round(rounds);
            rounds += 1;
        }
        Ok(())
    })?;
    let (in_kernel, preempted) = harness::take_tick_lock_stats();
    let elapsed = task::ticks() - start;
    check!(
        elapsed >= SOAK_TICKS,
        "only {elapsed} ticks in {rounds} rounds: the PIT did not run"
    );
    check!(
        in_kernel >= SOAK_TICKS / 2,
        "only {in_kernel} of {elapsed} ticks interrupted the workload"
    );
    check!(
        preempted == 0,
        "{preempted} of {in_kernel} ticks preempted a heap/console lock holder ({rounds} rounds)"
    );
    Ok(())
}

/// A `fmt::Write` sink into a heap string (tests only; the real report
/// writes to the UART).
struct Capture(String);

impl core::fmt::Write for Capture {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.push_str(s);
        Ok(())
    }
}

/// A report over a ring-0 context on this stack: every section present, the
/// lock line parseable, the kernel task listed, stack words dumped.
pub fn nmi_report_is_complete() -> Result<(), String> {
    kernel_task_only();
    let probe = 0u64;
    let at = Interrupted {
        rip: nmi_report_is_complete as *const () as u64,
        cs: 0x8,
        rflags: 0x2,
        rsp: core::ptr::addr_of!(probe) as u64 & !7,
    };
    let mut out = Capture(String::new());
    check!(nmi::report(&mut out, &at).is_ok(), "report failed");
    let text = out.0;
    for needle in [
        "HANG:BEGIN reason=nmi",
        "HANG:CPU rip=",
        " if=0 ",
        "HANG:LOCKS tasks=free heap=free console=free serial=free signals=free",
        "HANG:LASTTICK",
        "HANG:TASK slot=0 name=",
        "HANG:END",
    ] {
        check!(text.contains(needle), "report lacks {needle:?}:\n{text}");
    }
    let stack_lines = text.lines().filter(|l| l.starts_with("HANG:STACK")).count();
    check!(stack_lines >= 1, "no stack words dumped:\n{text}");
    Ok(())
}

/// Soak: many reports in a row leave the heap exactly as it was (the NMI
/// path must never allocate) and never leave a lock held.
pub fn soak_nmi_report_is_allocation_free() -> Result<(), String> {
    kernel_task_only();
    let at = Interrupted {
        rip: 0,
        cs: 0x2b,
        rflags: 0x202,
        rsp: 0,
    };
    // A fixed sink, so the only possible heap traffic is the report's own.
    struct Sink(usize);
    impl core::fmt::Write for Sink {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            self.0 += s.len();
            Ok(())
        }
    }
    let mut sink = Sink(0);
    let _ = crate::mem::heap_harness::take();
    for _ in 0..2_000 {
        check!(nmi::report(&mut sink, &at).is_ok(), "report failed");
    }
    let (sections, _) = crate::mem::heap_harness::take();
    check!(sections == 0, "2000 reports took {sections} heap sections");
    check!(sink.0 > 2_000 * 64, "reports were empty ({} bytes)", sink.0);
    check!(
        !crate::mem::heap_locked() && !task::diag::table_locked(),
        "a report left a lock held"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "preempt_console_lock_masks_interrupts",
        console_lock_masks_interrupts,
    ),
    (
        "preempt_heap_lock_masks_interrupts",
        heap_lock_masks_interrupts,
    ),
    (
        "preempt_soak_ticks_never_preempt_lock_holders",
        soak_ticks_never_preempt_lock_holders,
    ),
    ("preempt_nmi_report_is_complete", nmi_report_is_complete),
    (
        "preempt_soak_nmi_report_is_allocation_free",
        soak_nmi_report_is_allocation_free,
    ),
];
