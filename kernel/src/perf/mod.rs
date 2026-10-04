//! Latency instrumentation (docs/performance-plan.md, stage P0).
//!
//! Built only with `LAZYOS_PERF=1` (cfg `lazyos_perf`); without it every hook
//! below is an empty inline function. With it, the kernel measures:
//!
//! | Metric | From | To |
//! |---|---|---|
//! | `irq_wake` | the top half of a device or PS/2 interrupt | the task that interrupt woke is put on the CPU |
//! | `input_read` | a raw input record is published (PS/2 IRQ) | `inputd`'s raw-bus poll returns it |
//! | `input_present` | the same publication, for pointer records | the compositor's next `present` syscall returns |
//! | `irqoff` | a syscall enters (interrupts off) | it returns, parks, or naps (interrupts back on) |
//! | `ipc_rt` | `begin_call` of an in-kernel echo | `await_reply` returns its reply (no context switch) |
//! | `sleep_1ms` | the kernel task asks for a 1 ms sleep | the sleep returns |
//! | `present` | the display owner's `present` syscall starts | it returns (breaths between chunks included) |
//!
//! Durations are TSC cycles, converted with the PIT calibration when printed.
//! [`report`] runs from the kernel task and prints one line per metric that
//! changed:
//!
//! ```text
//! PERF:<metric>:n=<count> p50_us=<f> p90_us=<f> p99_us=<f> max_us=<f> mean_us=<f>
//! ```
//!
//! `tools/perf/run.py` parses the last line of each metric. Every report also
//! prints the per-task wake and switch counters (`PERF:sched`, [`wakeups`]),
//! which `tools/perf/idle.py` turns into an idle desktop's rates.
//!
//! Wake attribution: an interrupt sets the *chain stamp* (its TSC) for as long
//! as its handler runs, and the device bottom half sets it to the raising
//! interrupt's TSC while it posts that line's messages. Every task woken while
//! a chain stamp is set inherits it, and the scheduler records the delta when
//! that task next runs. A wake of the task already on the CPU (the claimant
//! was halted in its own wait loop) counts as running at once.

#[cfg(lazyos_perf)]
mod hist;
#[cfg(lazyos_perf)]
mod imp;
#[cfg(lazyos_perf)]
mod ipcbench;
#[cfg(lazyos_perf)]
mod sleepbench;
#[cfg(lazyos_perf)]
mod wakeups;

/// Read the time-stamp counter.
#[inline(always)]
#[allow(dead_code)]
pub fn rdtsc() -> u64 {
    // SAFETY: `rdtsc` only reads the time-stamp counter; no memory is touched.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// An interrupt handler starts: open a wake chain stamped now.
#[inline(always)]
pub fn irq_entry() {
    #[cfg(lazyos_perf)]
    imp::irq_entry();
}

/// The interrupt handler is done: close its wake chain.
#[inline(always)]
pub fn irq_exit() {
    #[cfg(lazyos_perf)]
    imp::irq_exit();
}

/// Device `line` was masked and queued for the bottom half.
#[inline(always)]
pub fn line_raised(_line: u8) {
    #[cfg(lazyos_perf)]
    imp::line_raised(_line);
}

/// The bottom half is about to post for the lines in `raised`.
#[inline(always)]
pub fn lines_posting(_raised: u16) {
    #[cfg(lazyos_perf)]
    imp::lines_posting(_raised);
}

/// The bottom half finished posting.
#[inline(always)]
pub fn lines_posted() {
    #[cfg(lazyos_perf)]
    imp::irq_exit();
}

/// Task `slot` moved from blocked to runnable; `current` is on the CPU.
#[inline(always)]
pub fn on_wake(_slot: usize, _current: usize) {
    #[cfg(lazyos_perf)]
    imp::on_wake(_slot, _current);
}

/// The scheduler is about to resume `slot`.
#[inline(always)]
pub fn on_run(_slot: usize) {
    #[cfg(lazyos_perf)]
    imp::on_run(_slot);
}

/// A raw input record was published (pointer motion when `pointer`).
#[inline(always)]
pub fn input_published(_pointer: bool) {
    #[cfg(lazyos_perf)]
    imp::input_published(_pointer);
}

/// A raw-bus consumer drained `count` records.
#[inline(always)]
pub fn input_read(_count: usize) {
    #[cfg(lazyos_perf)]
    imp::input_read(_count);
}

/// The display owner's `present`, begun at TSC `started`, returned.
#[inline(always)]
pub fn presented(_started: u64) {
    #[cfg(lazyos_perf)]
    imp::presented(_started);
}

/// A syscall entered with interrupts off (`nr` for the report).
#[inline(always)]
pub fn syscall_entry(_nr: u64) {
    #[cfg(lazyos_perf)]
    imp::syscall_entry(_nr);
}

/// The syscall returns to user mode.
#[inline(always)]
pub fn syscall_exit() {
    #[cfg(lazyos_perf)]
    imp::syscall_exit();
}

/// Interrupts are about to come on (a nap or a park): end the stretch.
#[inline(always)]
pub fn irqoff_pause() {
    #[cfg(lazyos_perf)]
    imp::irqoff_pause();
}

/// Interrupts are off again inside the same syscall: start a new stretch.
#[inline(always)]
pub fn irqoff_resume() {
    #[cfg(lazyos_perf)]
    imp::irqoff_resume();
}

/// Print every metric that changed (kernel task, periodically).
#[inline(always)]
pub fn service() {
    #[cfg(lazyos_perf)]
    imp::service();
}
