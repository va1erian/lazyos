//! Interrupts-off accounting per syscall: how long each syscall kept the CPU
//! deaf to interrupts at a stretch, worst case, since boot.
//!
//! A *span* starts wherever a syscall's code begins to run with interrupts
//! off (the gate's entry, the return from a [`super::irq_window`], a `nap` or
//! a park) and ends wherever they come back on (a window, `nap`, a park, the
//! return to user mode). Each span is charged to the syscall the current task
//! is in; the per-syscall maximum is kept, and each new maximum of
//! [`REPORT_US`] or more is logged as
//! `IRQOFF:MAX abi=<native|linux|kernel> nr=<n> us=<n> over=<n> missed_ticks=<n>
//! dropped=<n> from=<file:line> to=<file:line>` (`over`: spans past the bound so far;
//! `missed_ticks`: timer periods the clock had to catch up, `arch::clock`;
//! `from`/`to`: where the span began and ended, so the stretch that lacks a
//! poll point lies between those two lines). This generalises the i8042's
//! `PS2:GAP` to every syscall; `irq_window` is what keeps the spans short.
//!
//! A report is queued when the span closes and written when the next span
//! starts, where each byte the UART takes is a poll point again: writing it
//! inside `close` (interrupts off, no span open) would itself be a long
//! stretch at a real UART's baud rate. A report that finds the serial port
//! busy stays queued for the next span; past [`QUEUE_LEN`] waiting ones the
//! oldest is dropped and counted (`dropped=` on the next report).
//!
//! Under a hypervisor a span also contains any time the host did not run the
//! vCPU, so a record in a trivial syscall on a loaded host is noise; a
//! stretch really missing a poll point shows up in every run.
//!
//! Spans outside syscalls (the kernel task's own `without_interrupts`
//! sections, interrupt handlers) are not charged here.

use core::panic::Location;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use spin::Mutex;

use crate::task::MAX_TASKS;

/// Spans this long or longer break the latency bound and are logged.
pub const REPORT_US: u64 = 2_000;
/// Native (`int 0x80`) syscall numbers tracked; higher ones share the last.
pub const NATIVE_SLOTS: usize = 64;
/// Linux syscall numbers tracked; higher ones share the last.
pub const LINUX_SLOTS: usize = 512;

/// Tag for a native syscall number (the same bit `process::gate` uses).
const NATIVE: u64 = 1 << 63;
/// Tag for a kernel section ([`kernel_section`]).
const KERNEL: u64 = 1 << 62;
/// Marks "no syscall" in [`TASK_NR`] and [`SPAN_NR`].
const NONE: u64 = u64::MAX;

type Site = Location<'static>;

/// Worst span per syscall, in TSC cycles.
static MAX_NATIVE: [AtomicU64; NATIVE_SLOTS] = [const { AtomicU64::new(0) }; NATIVE_SLOTS];
static MAX_LINUX: [AtomicU64; LINUX_SLOTS] = [const { AtomicU64::new(0) }; LINUX_SLOTS];
static MAX_KERNEL: AtomicU64 = AtomicU64::new(0);
/// The syscall each task is in (tagged), [`NONE`] outside one.
static TASK_NR: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(NONE) }; MAX_TASKS];
/// The open span: its start (0 = none), the syscall it is charged to and
/// where it began.
static SPAN_START: AtomicU64 = AtomicU64::new(0);
static SPAN_NR: AtomicU64 = AtomicU64::new(NONE);
static SPAN_FROM: AtomicPtr<Site> = AtomicPtr::new(core::ptr::null_mut());
/// The task whose syscall the open span belongs to.
static SPAN_TASK: AtomicU64 = AtomicU64::new(0);
/// Spans that reached [`REPORT_US`].
static OVER: AtomicU64 = AtomicU64::new(0);

/// Reports waiting for the next span.
const QUEUE_LEN: usize = 8;

/// One new maximum to log.
#[derive(Clone, Copy)]
struct Report {
    tagged: u64,
    us: u64,
    over: u64,
    missed: u64,
    from: &'static Site,
    to: &'static Site,
}

/// Queued reports, oldest first, and reports dropped from a full queue.
/// Taken only with interrupts off and never while printing.
static QUEUE: Mutex<([Option<Report>; QUEUE_LEN], u64)> = Mutex::new(([None; QUEUE_LEN], 0));
/// Set while [`report_queued`] runs: a span its own printing starts must not
/// report again.
static REPORTING: AtomicBool = AtomicBool::new(false);

/// The gate entered native syscall `nr`: its first span starts.
#[track_caller]
pub fn enter_native(nr: u64) {
    enter(NATIVE | nr, Location::caller());
}

/// The gate entered Linux syscall `nr`.
#[track_caller]
pub fn enter_linux(nr: u64) {
    enter(nr, Location::caller());
}

fn enter(tagged: u64, at: &'static Site) {
    TASK_NR[crate::task::current()].store(tagged, Ordering::Relaxed);
    start(tagged, at);
}

/// Run `f`, interrupts-off work of the kernel task (the periodic writeback,
/// disk statistics), as a span of its own: its poll points open interrupt
/// windows as a syscall's do, and its stretches are logged as `abi=kernel`.
/// Call with interrupts off from task context, never from an interrupt
/// handler.
#[track_caller]
pub fn kernel_section<R>(f: impl FnOnce() -> R) -> R {
    enter(KERNEL, Location::caller());
    let result = f();
    exit();
    result
}

/// The syscall returns to user mode: its last span ends.
#[track_caller]
pub fn exit() {
    close();
    TASK_NR[crate::task::current()].store(NONE, Ordering::Relaxed);
}

/// Interrupts go back on (or the task gives up the CPU): end the open span.
#[track_caller]
pub fn close() {
    let start = SPAN_START.swap(0, Ordering::Relaxed);
    let nr = SPAN_NR.load(Ordering::Relaxed);
    if start == 0 || nr == NONE {
        return;
    }
    let from = SPAN_FROM.load(Ordering::Relaxed);
    record(nr, rdtsc().wrapping_sub(start), from, Location::caller());
}

/// Whether the current task's syscall has an interrupts-off span open (we
/// are in that syscall, not in a handler that interrupted user mode, a sleep
/// or another task). The task check is a second line of defence: every path
/// that gives up the CPU closes the span first ([`paused`]).
#[inline]
pub fn span_open() -> bool {
    SPAN_START.load(Ordering::Relaxed) != 0
        && SPAN_TASK.load(Ordering::Relaxed) == crate::task::current() as u64
}

/// Run `f`, which lets interrupts in or gives up the CPU (a voluntary
/// switch, a halt), outside the current span: the span ends before and,
/// only if one was open, starts again after. A switch from an interrupt
/// handler or an exit path therefore opens nothing for the task it returns
/// to.
#[track_caller]
pub fn paused<R>(f: impl FnOnce() -> R) -> R {
    let charged = span_open();
    close();
    let result = f();
    if charged {
        resume();
    }
    result
}

/// Interrupts are off again for the current task (after a window, a `nap`
/// or a park, possibly on another task's behalf after a switch).
#[track_caller]
pub fn resume() {
    let tagged = TASK_NR[crate::task::current()].load(Ordering::Relaxed);
    start(tagged, Location::caller());
}

fn start(tagged: u64, at: &'static Site) {
    SPAN_NR.store(tagged, Ordering::Relaxed);
    if tagged == NONE {
        SPAN_START.store(0, Ordering::Relaxed);
        return;
    }
    SPAN_FROM.store(at as *const Site as *mut Site, Ordering::Relaxed);
    SPAN_TASK.store(crate::task::current() as u64, Ordering::Relaxed);
    let now = rdtsc();
    SPAN_START.store(now, Ordering::Relaxed);
    super::irq_window::restart(now);
    report_queued();
}

fn record(tagged: u64, cycles: u64, from: *const Site, to: &'static Site) {
    let slot = slot_of(tagged);
    let us = to_us(cycles);
    let over_bound = us >= REPORT_US;
    let over = if over_bound {
        OVER.fetch_add(1, Ordering::Relaxed) + 1
    } else {
        OVER.load(Ordering::Relaxed)
    };
    if cycles <= slot.load(Ordering::Relaxed) {
        return;
    }
    slot.store(cycles, Ordering::Relaxed);
    if over_bound {
        // SAFETY: `SPAN_FROM` only ever holds null or a `&'static Location`.
        let from = unsafe { from.as_ref() }.unwrap_or(to);
        let missed = super::clock::missed_ticks();
        enqueue(Report {
            tagged,
            us,
            over,
            missed,
            from,
            to,
        });
    }
}

/// Queue `report`, dropping (and counting) the oldest when full.
fn enqueue(report: Report) {
    let mut queue = QUEUE.lock();
    let (slots, dropped) = &mut *queue;
    if slots[QUEUE_LEN - 1].is_some() {
        slots.rotate_left(1);
        slots[QUEUE_LEN - 1] = None;
        *dropped += 1;
    }
    if let Some(free) = slots.iter_mut().find(|slot| slot.is_none()) {
        *free = Some(report);
    }
}

/// Write the queued reports, oldest first, stopping (and keeping the rest)
/// when the serial port is busy.
fn report_queued() {
    if REPORTING.swap(true, Ordering::Relaxed) {
        return;
    }
    loop {
        let (next, dropped) = {
            let queue = QUEUE.lock();
            (queue.0[0], queue.1)
        };
        let Some(report) = next else { break };
        let (abi, nr) = split(report.tagged);
        let written = crate::serial::try_print(format_args!(
            "IRQOFF:MAX abi={abi} nr={nr} us={} over={} missed_ticks={} dropped={dropped} from={}:{} to={}:{}\n",
            report.us,
            report.over,
            report.missed,
            report.from.file(),
            report.from.line(),
            report.to.file(),
            report.to.line(),
        ));
        if !written {
            break;
        }
        let mut queue = QUEUE.lock();
        queue.0.rotate_left(1);
        queue.0[QUEUE_LEN - 1] = None;
        queue.1 = 0;
    }
    REPORTING.store(false, Ordering::Relaxed);
}

fn slot_of(tagged: u64) -> &'static AtomicU64 {
    match split(tagged) {
        ("native", nr) => &MAX_NATIVE[(nr as usize).min(NATIVE_SLOTS - 1)],
        ("kernel", _) => &MAX_KERNEL,
        (_, nr) => &MAX_LINUX[(nr as usize).min(LINUX_SLOTS - 1)],
    }
}

fn split(tagged: u64) -> (&'static str, u64) {
    if tagged & NATIVE != 0 {
        ("native", tagged & !NATIVE)
    } else if tagged & KERNEL != 0 {
        ("kernel", tagged & !KERNEL)
    } else {
        ("linux", tagged)
    }
}

/// Cycles to microseconds (a timer period is 10 ms); 0 if uncalibrated.
pub fn to_us(cycles: u64) -> u64 {
    let per_tick = super::clock::cycles_per_tick();
    if per_tick == 0 {
        return 0;
    }
    (cycles as u128 * 10_000 / per_tick as u128) as u64
}

/// Worst span charged to native syscall `nr`, in microseconds.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn max_native_us(nr: u64) -> u64 {
    to_us(slot_of(NATIVE | nr).load(Ordering::Relaxed))
}

/// Spans that reached [`REPORT_US`] since boot (or [`reset`]).
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn over_bound() -> u64 {
    OVER.load(Ordering::Relaxed)
}

/// Test hook: forget every maximum.
#[cfg(lazyos_tests)]
pub fn reset() {
    for slot in MAX_NATIVE.iter().chain(MAX_LINUX.iter()) {
        slot.store(0, Ordering::Relaxed);
    }
    MAX_KERNEL.store(0, Ordering::Relaxed);
    OVER.store(0, Ordering::Relaxed);
}

fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` reads a CPU counter; no memory or privilege effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}
