//! Latency sample rings for the `PERF:` report.
//!
//! A [`Samples`] keeps the newest [`CAPACITY`] durations (TSC cycles) of one
//! metric plus a total count and the maximum ever seen. Recording is a few
//! relaxed atomic stores, so it is safe from interrupt context and from
//! syscalls running with interrupts off; this single CPU never records two
//! samples of one metric at once (every recorder runs with interrupts off).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// Samples retained per metric for the percentiles.
pub const CAPACITY: usize = 4096;

/// One metric's samples.
pub struct Samples {
    ring: [AtomicU64; CAPACITY],
    count: AtomicU64,
    max: AtomicU64,
    /// `count` at the last report, so an unchanged metric is not reprinted.
    reported: AtomicU64,
}

impl Samples {
    pub const fn new() -> Samples {
        Samples {
            ring: [const { AtomicU64::new(0) }; CAPACITY],
            count: AtomicU64::new(0),
            max: AtomicU64::new(0),
            reported: AtomicU64::new(0),
        }
    }

    /// Record one duration in cycles.
    pub fn record(&self, cycles: u64) {
        let index = self.count.fetch_add(1, Ordering::Relaxed) as usize % CAPACITY;
        self.ring[index].store(cycles, Ordering::Relaxed);
        self.max.fetch_max(cycles, Ordering::Relaxed);
    }

    /// Total samples recorded since boot.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Whether samples arrived since the last [`Samples::mark_reported`].
    pub fn changed(&self) -> bool {
        self.count() != self.reported.load(Ordering::Relaxed)
    }

    pub fn mark_reported(&self) {
        self.reported.store(self.count(), Ordering::Relaxed);
    }

    /// A summary of the retained samples, or `None` when there are none.
    pub fn summary(&self) -> Option<Summary> {
        let count = self.count();
        if count == 0 {
            return None;
        }
        let kept = (count as usize).min(CAPACITY);
        let mut values: Vec<u64> = self.ring[..kept]
            .iter()
            .map(|value| value.load(Ordering::Relaxed))
            .collect();
        values.sort_unstable();
        let pick = |permille: usize| values[((kept - 1) * permille) / 1000];
        let sum: u128 = values.iter().map(|&value| u128::from(value)).sum();
        Some(Summary {
            count,
            p50: pick(500),
            p90: pick(900),
            p99: pick(990),
            max: self.max.load(Ordering::Relaxed),
            mean: (sum / kept as u128) as u64,
        })
    }
}

/// Percentiles over the retained samples (cycles), `max` over all of them.
#[derive(Clone, Copy, Debug)]
pub struct Summary {
    pub count: u64,
    pub p50: u64,
    pub p90: u64,
    pub p99: u64,
    pub max: u64,
    pub mean: u64,
}

/// Cycles to nanoseconds with the PIT-calibrated TSC rate (`per_tick` cycles
/// per 10 ms); 0 when the TSC is uncalibrated.
pub fn cycles_to_ns(cycles: u64, per_tick: u64) -> u64 {
    if per_tick == 0 {
        return 0;
    }
    (u128::from(cycles) * 10_000_000 / u128::from(per_tick)) as u64
}
