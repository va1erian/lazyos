//! Boot-phase timestamps for the boot-time benchmark (`tools/bench/boot_time.py`).
//!
//! Each [`mark`] prints one stable serial line, `BOOT:PHASE:<name>:tsc=<n>`,
//! carrying the CPU time-stamp counter. The TSC is read straight from the CPU,
//! so the deltas mean the same thing under TCG and under WHPX/KVM and need no
//! calibration (the PIT tick counter does not exist before `arch::init`, which
//! is itself a phase worth timing). The host benchmark reports the deltas in
//! Mcycles next to its own wall-clock milestones.
//!
//! A small ring of the most recent stamps is kept too, so tests (and a future
//! `/proc`-style consumer) can read phases back without parsing serial.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// How many recent stamps the ring keeps.
const RING: usize = 32;

static COUNT: AtomicUsize = AtomicUsize::new(0);
static STAMPS: [AtomicU64; RING] = [const { AtomicU64::new(0) }; RING];

/// The CPU time-stamp counter.
pub fn tsc() -> u64 {
    // SAFETY: `rdtsc` reads a CPU counter; it has no memory or privilege
    // requirements and every x86_64 CPU implements it.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Store `stamp` in the ring and return its sequence number.
pub fn record(stamp: u64) -> usize {
    let index = COUNT.fetch_add(1, Ordering::Relaxed);
    STAMPS[index % RING].store(stamp, Ordering::Relaxed);
    index
}

/// How many stamps have been recorded since boot.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // read back by tests only
pub fn count() -> usize {
    COUNT.load(Ordering::Relaxed)
}

/// The stamp recorded as sequence number `index`, while it is still in the
/// ring (the newest [`RING`] stamps).
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // read back by tests only
pub fn stamp(index: usize) -> Option<u64> {
    let newest = COUNT.load(Ordering::Relaxed);
    if index >= newest || newest - index > RING {
        return None;
    }
    Some(STAMPS[index % RING].load(Ordering::Relaxed))
}

/// Timestamp a boot phase and print its serial line. `name` must be
/// `[A-Za-z0-9_]` so the host parser's pattern matches it.
pub fn mark(args: core::fmt::Arguments) {
    let stamp = tsc();
    record(stamp);
    serial_println!("BOOT:PHASE:{}:tsc={}", args, stamp);
}

/// `boot_phase!("name")` or `boot_phase!("read_{}", file)`.
macro_rules! boot_phase {
    ($($arg:tt)*) => {
        $crate::boot_trace::mark(format_args!($($arg)*))
    };
}
