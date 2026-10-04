//! The recording side of `perf` (compiled only with cfg `lazyos_perf`).

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::hist::{cycles_to_ns, Samples, Summary};
use super::rdtsc;
use crate::task::{self, MAX_TASKS};

static IRQ_WAKE: Samples = Samples::new();
static INPUT_READ: Samples = Samples::new();
static INPUT_PRESENT: Samples = Samples::new();
static IRQOFF: Samples = Samples::new();
static IPC_RT: Samples = Samples::new();
static SLEEP_1MS: Samples = Samples::new();
static PRESENT: Samples = Samples::new();
/// How long one report took to print, and the `input_present` samples whose
/// interval contained a report: what the harness itself costs the input path.
static REPORT: Samples = Samples::new();
static INPUT_PRESENT_RPT: Samples = Samples::new();
/// TSC at the end of the last report (0: none yet).
static LAST_REPORT_END: AtomicU64 = AtomicU64::new(0);

/// TSC of the interrupt whose consequences are running now (0: none).
static CHAIN: AtomicU64 = AtomicU64::new(0);
/// TSC of each device line's first unserviced raise.
static LINE_TSC: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
/// The chain stamp a woken task carries until it runs.
static WAKE_STAMP: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

/// Oldest raw record not yet read by a consumer, and the oldest pointer
/// record read but not yet presented.
static INPUT_PENDING: AtomicU64 = AtomicU64::new(0);
static POINTER_PENDING: AtomicU64 = AtomicU64::new(0);
static POINTER_READ: AtomicU64 = AtomicU64::new(0);

/// Start of the open interrupts-off stretch (0: none), and which tasks are
/// inside a syscall.
static IRQOFF_START: AtomicU64 = AtomicU64::new(0);
static IN_SYSCALL: [AtomicBool; MAX_TASKS] = [const { AtomicBool::new(false) }; MAX_TASKS];
static SYSCALL_NR: AtomicU64 = AtomicU64::new(0);
static WORST_IRQOFF: AtomicU64 = AtomicU64::new(0);
static WORST_NR: AtomicU64 = AtomicU64::new(0);

/// Samples older than this are attribution mistakes (a stamp left on a slot
/// that died and was reused), not latencies: one second at any sane TSC rate
/// is far beyond it, so drop them.
const MAX_PLAUSIBLE_CYCLES: u64 = 1 << 34;

pub fn irq_entry() {
    CHAIN.store(rdtsc(), Ordering::Relaxed);
}

pub fn irq_exit() {
    CHAIN.store(0, Ordering::Relaxed);
}

pub fn line_raised(line: u8) {
    if let Some(stamp) = LINE_TSC.get(usize::from(line)) {
        let _ = stamp.compare_exchange(0, rdtsc(), Ordering::Relaxed, Ordering::Relaxed);
    }
}

pub fn lines_posting(raised: u16) {
    let mut oldest = 0u64;
    for (line, stamp) in LINE_TSC.iter().enumerate() {
        if raised & (1 << line) == 0 {
            continue;
        }
        let value = stamp.swap(0, Ordering::Relaxed);
        if value != 0 && (oldest == 0 || value < oldest) {
            oldest = value;
        }
    }
    CHAIN.store(oldest, Ordering::Relaxed);
}

pub fn on_wake(slot: usize, current: usize) {
    let chain = CHAIN.load(Ordering::Relaxed);
    if chain == 0 {
        return;
    }
    if slot == current {
        // Halted in its own wait loop: it runs as soon as the handler returns.
        record(&IRQ_WAKE, rdtsc().wrapping_sub(chain));
        return;
    }
    if let Some(stamp) = WAKE_STAMP.get(slot) {
        let _ = stamp.compare_exchange(0, chain, Ordering::Relaxed, Ordering::Relaxed);
    }
}

pub fn on_run(slot: usize) {
    let Some(stamp) = WAKE_STAMP.get(slot) else {
        return;
    };
    let chain = stamp.swap(0, Ordering::Relaxed);
    if chain != 0 {
        record(&IRQ_WAKE, rdtsc().wrapping_sub(chain));
    }
}

pub fn input_published(pointer: bool) {
    LAST_INPUT_TICK.store(task::ticks(), Ordering::Relaxed);
    let now = rdtsc();
    let _ = INPUT_PENDING.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
    if pointer {
        let _ = POINTER_PENDING.compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed);
    }
}

pub fn input_read(count: usize) {
    if count == 0 {
        return;
    }
    let published = INPUT_PENDING.swap(0, Ordering::Relaxed);
    if published != 0 {
        record(&INPUT_READ, rdtsc().wrapping_sub(published));
    }
    let pointer = POINTER_PENDING.swap(0, Ordering::Relaxed);
    if pointer != 0 {
        let _ = POINTER_READ.compare_exchange(0, pointer, Ordering::Relaxed, Ordering::Relaxed);
    }
}

pub fn presented(started: u64) {
    record(&PRESENT, rdtsc().wrapping_sub(started));
    let pointer = POINTER_READ.swap(0, Ordering::Relaxed);
    if pointer != 0 {
        let cycles = rdtsc().wrapping_sub(pointer);
        record(&INPUT_PRESENT, cycles);
        if LAST_REPORT_END.load(Ordering::Relaxed) > pointer {
            record(&INPUT_PRESENT_RPT, cycles);
        }
    }
}

pub fn syscall_entry(nr: u64) {
    if let Some(flag) = IN_SYSCALL.get(task::current()) {
        flag.store(true, Ordering::Relaxed);
    }
    SYSCALL_NR.store(nr, Ordering::Relaxed);
    IRQOFF_START.store(rdtsc(), Ordering::Relaxed);
}

pub fn syscall_exit() {
    irqoff_pause();
    if let Some(flag) = IN_SYSCALL.get(task::current()) {
        flag.store(false, Ordering::Relaxed);
    }
}

pub fn irqoff_pause() {
    let start = IRQOFF_START.swap(0, Ordering::Relaxed);
    // Only a stretch the current task opened inside its own syscall counts: a
    // syscall that never returns (`exit` halting with interrupts on) leaves
    // its start behind, and the next pause elsewhere must not book that gap.
    let inside = IN_SYSCALL
        .get(task::current())
        .is_some_and(|flag| flag.load(Ordering::Relaxed));
    if start == 0 || !inside {
        return;
    }
    let cycles = rdtsc().wrapping_sub(start);
    if cycles > MAX_PLAUSIBLE_CYCLES {
        return;
    }
    IRQOFF.record(cycles);
    if cycles > WORST_IRQOFF.load(Ordering::Relaxed) {
        WORST_IRQOFF.store(cycles, Ordering::Relaxed);
        WORST_NR.store(SYSCALL_NR.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}

pub fn irqoff_resume() {
    let inside = IN_SYSCALL
        .get(task::current())
        .is_some_and(|flag| flag.load(Ordering::Relaxed));
    if inside {
        IRQOFF_START.store(rdtsc(), Ordering::Relaxed);
    }
}

fn record(samples: &Samples, cycles: u64) {
    if cycles <= MAX_PLAUSIBLE_CYCLES {
        samples.record(cycles);
    }
}

/// Ticks between two reports.
const REPORT_TICKS: u64 = 200;
/// The in-kernel IPC benchmark runs once, this long after boot, so it does
/// not compete with service start-up.
const IPC_BENCH_TICK: u64 = 1500;
static NEXT_REPORT: AtomicU64 = AtomicU64::new(0);
static IPC_DONE: AtomicBool = AtomicBool::new(false);
/// The sleep benchmark runs once, after the IPC one.
const SLEEP_BENCH_TICK: u64 = 1700;
static SLEEP_DONE: AtomicBool = AtomicBool::new(false);
static REPORTED_WORST: AtomicU64 = AtomicU64::new(0);

/// A report waits until input has been quiet this long (ticks)...
const QUIET_TICKS: u64 = 20;
/// ...but never longer than this past its due time (ticks).
const MAX_DEFER_TICKS: u64 = 1000;
/// Tick of the newest raw input record.
static LAST_INPUT_TICK: AtomicU64 = AtomicU64::new(0);

/// Whether the report due at `due` should wait: printing it takes several
/// milliseconds of polled serial output (`PERF:report`), during which the
/// kernel task keeps the CPU, so a report in the middle of an input burst
/// lands in the very latencies it reports (`PERF:input_present_rpt`).
fn defer_report(now: u64, due: u64) -> bool {
    let quiet = now.saturating_sub(LAST_INPUT_TICK.load(Ordering::Relaxed)) >= QUIET_TICKS;
    !quiet && now < due + MAX_DEFER_TICKS
}

pub fn service() {
    let now = task::ticks();
    let due = NEXT_REPORT.load(Ordering::Relaxed);
    if now < due {
        return;
    }
    if now >= IPC_BENCH_TICK && !IPC_DONE.swap(true, Ordering::Relaxed) {
        super::ipcbench::run(|cycles| IPC_RT.record(cycles));
    }
    if now >= SLEEP_BENCH_TICK && !SLEEP_DONE.swap(true, Ordering::Relaxed) {
        super::sleepbench::run(|cycles| SLEEP_1MS.record(cycles));
    }
    if due != 0 && defer_report(now, due) {
        return;
    }
    NEXT_REPORT.store(now + REPORT_TICKS, Ordering::Relaxed);
    let per_tick = crate::arch::clock::cycles_per_tick();
    let started = rdtsc();
    for (name, samples) in [
        ("irq_wake", &IRQ_WAKE),
        ("input_read", &INPUT_READ),
        ("input_present", &INPUT_PRESENT),
        ("irqoff", &IRQOFF),
        ("ipc_rt", &IPC_RT),
        ("sleep_1ms", &SLEEP_1MS),
        ("present", &PRESENT),
        ("report", &REPORT),
        ("input_present_rpt", &INPUT_PRESENT_RPT),
    ] {
        if !samples.changed() {
            continue;
        }
        samples.mark_reported();
        if let Some(summary) = samples.summary() {
            print(name, &summary, per_tick);
        }
    }
    let worst = WORST_IRQOFF.load(Ordering::Relaxed);
    if worst != 0 && worst != REPORTED_WORST.swap(worst, Ordering::Relaxed) {
        let nr = WORST_NR.load(Ordering::Relaxed);
        line(format_args!(
            "PERF:irqoff_worst:us={} syscall={nr:#x}\n",
            Micros(cycles_to_ns(worst, per_tick))
        ));
    }
    let end = rdtsc();
    REPORT.record(end.wrapping_sub(started));
    LAST_REPORT_END.store(end, Ordering::Relaxed);
}

fn print(name: &str, summary: &Summary, per_tick: u64) {
    let us = |cycles| Micros(cycles_to_ns(cycles, per_tick));
    line(format_args!(
        "PERF:{name}:n={} p50_us={} p90_us={} p99_us={} max_us={} mean_us={}\n",
        summary.count,
        us(summary.p50),
        us(summary.p90),
        us(summary.p99),
        us(summary.max),
        us(summary.mean),
    ));
}

/// Print with interrupts off: an interrupt handler that prints uses
/// `try_print`, and must find the port free rather than held by this task.
fn line(args: core::fmt::Arguments) {
    x86_64::instructions::interrupts::without_interrupts(|| crate::serial::_print(args));
}

/// Nanoseconds shown as microseconds with one decimal.
struct Micros(u64);

impl core::fmt::Display for Micros {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}", self.0 / 1000, (self.0 % 1000) / 100)
    }
}
