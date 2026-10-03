//! Interrupts-off accounting per syscall: how long each syscall kept the CPU
//! deaf to interrupts at a stretch, worst case, since boot.
//!
//! A *span* starts wherever a syscall's code begins to run with interrupts
//! off (the gate's entry, the return from a [`super::irq_window`], a `nap` or
//! a park) and ends wherever they come back on (a window, `nap`, a park, the
//! return to user mode). Each span is charged to the syscall the current task
//! is in; the per-syscall maximum is kept, and each new maximum of
//! [`REPORT_US`] or more is logged as
//! `IRQOFF:MAX abi=<native|linux> nr=<n> us=<n> over=<n> missed_ticks=<n>
//! from=<file:line> to=<file:line>` (`over`: spans past the bound so far;
//! `missed_ticks`: timer periods the clock had to catch up, `arch::clock`;
//! `from`/`to`: where the span began and ended, so the stretch that lacks a
//! poll point lies between those two lines). This generalises the i8042's
//! `PS2:GAP` to every syscall; `irq_window` is what keeps the spans short.
//!
//! Under a hypervisor a span also contains any time the host did not run the
//! vCPU, so a record in a trivial syscall on a loaded host is noise; a
//! stretch really missing a poll point shows up in every run.
//!
//! Spans outside syscalls (the kernel task's own `without_interrupts`
//! sections, interrupt handlers) are not charged here.

use core::panic::Location;
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use crate::task::MAX_TASKS;

/// Spans this long or longer break the latency bound and are logged.
pub const REPORT_US: u64 = 2_000;
/// Native (`int 0x80`) syscall numbers tracked; higher ones share the last.
pub const NATIVE_SLOTS: usize = 64;
/// Linux syscall numbers tracked; higher ones share the last.
pub const LINUX_SLOTS: usize = 512;

/// Tag for a native syscall number (the same bit `process::gate` uses).
const NATIVE: u64 = 1 << 63;
/// Marks "no syscall" in [`TASK_NR`] and [`SPAN_NR`].
const NONE: u64 = u64::MAX;

type Site = Location<'static>;

/// Worst span per syscall, in TSC cycles.
static MAX_NATIVE: [AtomicU64; NATIVE_SLOTS] = [const { AtomicU64::new(0) }; NATIVE_SLOTS];
static MAX_LINUX: [AtomicU64; LINUX_SLOTS] = [const { AtomicU64::new(0) }; LINUX_SLOTS];
/// The syscall each task is in (tagged), [`NONE`] outside one.
static TASK_NR: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(NONE) }; MAX_TASKS];
/// The open span: its start (0 = none), the syscall it is charged to and
/// where it began.
static SPAN_START: AtomicU64 = AtomicU64::new(0);
static SPAN_NR: AtomicU64 = AtomicU64::new(NONE);
static SPAN_FROM: AtomicPtr<Site> = AtomicPtr::new(core::ptr::null_mut());
/// Spans that reached [`REPORT_US`].
static OVER: AtomicU64 = AtomicU64::new(0);

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

/// Whether a syscall's interrupts-off span is open (we are in a syscall, not
/// in a handler that interrupted user mode or a sleep).
#[inline]
pub fn span_open() -> bool {
    SPAN_START.load(Ordering::Relaxed) != 0
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
    let now = rdtsc();
    SPAN_START.store(now, Ordering::Relaxed);
    super::irq_window::restart(now);
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
        let (abi, nr) = split(tagged);
        // SAFETY: `SPAN_FROM` only ever holds null or a `&'static Location`.
        let from = unsafe { from.as_ref() }.unwrap_or(to);
        // The serial lock may be held by the code this span ran; the record
        // stays in the table either way.
        let _ = crate::serial::try_print(format_args!(
            "IRQOFF:MAX abi={abi} nr={nr} us={us} over={over} missed_ticks={} from={}:{} to={}:{}\n",
            super::clock::missed_ticks(),
            from.file(),
            from.line(),
            to.file(),
            to.line(),
        ));
    }
}

fn slot_of(tagged: u64) -> &'static AtomicU64 {
    match split(tagged) {
        ("native", nr) => &MAX_NATIVE[(nr as usize).min(NATIVE_SLOTS - 1)],
        (_, nr) => &MAX_LINUX[(nr as usize).min(LINUX_SLOTS - 1)],
    }
}

fn split(tagged: u64) -> (&'static str, u64) {
    if tagged & NATIVE != 0 {
        ("native", tagged & !NATIVE)
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
    OVER.store(0, Ordering::Relaxed);
}

fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` reads a CPU counter; no memory or privilege effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}
