//! The kernel-task sleep benchmark behind `PERF:sleep_1ms`.
//!
//! The kernel task asks for a 1 ms sleep [`ROUNDS`] times and records how
//! long each one really took, in TSC cycles. It parks on the same sleep path
//! every timed wait uses, so the figure is the timer's own resolution: the
//! wait's rounding plus the delay between the deadline and the wake.

/// Sleeps per run (warm-up excluded).
const ROUNDS: usize = 200;
const WARMUP: usize = 5;

/// Sleep for one millisecond the way a 1 ms timeout is honoured: a
/// monotonic-nanosecond deadline (P2), ended by the deadline timer.
fn sleep_1ms() {
    crate::task::idle_ns(crate::arch::clock::monotonic_ns() + 1_000_000);
}

/// Run the benchmark, handing each sleep's cycles to `record`.
pub fn run(mut record: impl FnMut(u64)) {
    for round in 0..WARMUP + ROUNDS {
        let start = super::rdtsc();
        sleep_1ms();
        let cycles = super::rdtsc().wrapping_sub(start);
        if round >= WARMUP {
            record(cycles);
        }
    }
}
