//! CPU load from two snapshots: the busy share of the ticks between them.

use super::Snapshot;

/// The two counters CPU load is computed from: uptime and the ticks the
/// scheduler found the CPU idle, both in 100 Hz PIT ticks.
///
/// Idle time comes from the kernel's own counter rather than from summing
/// per-task CPU ticks: the kernel has no idle task, so a summed figure would
/// count the ticks spent halted inside a parked task's wait loop as busy and
/// read 100 % on a quiet system.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CpuSample {
    /// PIT ticks since boot.
    pub ticks: u64,
    /// PIT ticks that found the CPU idle.
    pub idle: u64,
}

impl Snapshot {
    /// The counters [`cpu_percent`] compares between two snapshots.
    pub fn cpu_sample(&self) -> CpuSample {
        CpuSample {
            ticks: self.ticks,
            idle: self.idle_ticks,
        }
    }
}

/// CPU load in whole percent (0..=100) between two samples.
///
/// Both counters are diffed with `wrapping_sub`, so wrapped counters still
/// give the right interval. Idle is clamped to the elapsed ticks (the two
/// words are read at slightly different moments), so the busy share never
/// goes negative or above 100. No elapsed ticks is 0 rather than a division
/// by zero.
pub fn cpu_percent(prev: CpuSample, cur: CpuSample) -> u32 {
    let elapsed = cur.ticks.wrapping_sub(prev.ticks);
    if elapsed == 0 {
        return 0;
    }
    let idle = cur.idle.wrapping_sub(prev.idle).min(elapsed);
    let busy = elapsed - idle;
    (u128::from(busy) * 100 / u128::from(elapsed)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysinfo::{decode_words, header, TaskRow, TaskState, VERSION, WORDS};

    const fn sample(ticks: u64, idle: u64) -> CpuSample {
        CpuSample { ticks, idle }
    }

    #[test]
    fn half_idle_is_fifty_percent() {
        assert_eq!(cpu_percent(sample(100, 10), sample(200, 60)), 50);
    }

    #[test]
    fn a_fully_idle_interval_is_zero_percent() {
        assert_eq!(cpu_percent(sample(100, 40), sample(200, 140)), 0);
    }

    #[test]
    fn no_idle_ticks_is_one_hundred_percent() {
        assert_eq!(cpu_percent(sample(100, 40), sample(200, 40)), 100);
    }

    #[test]
    fn no_elapsed_ticks_is_zero_not_a_division_by_zero() {
        assert_eq!(cpu_percent(sample(500, 5), sample(500, 50)), 0);
    }

    #[test]
    fn wrapped_counters_still_measure_the_interval() {
        let prev = sample(u64::MAX - 49, u64::MAX - 24);
        let cur = sample(50, 25);
        // 100 ticks elapsed across the wrap, 50 of them idle.
        assert_eq!(cpu_percent(prev, cur), 50);
    }

    #[test]
    fn idle_is_clamped_to_the_elapsed_ticks() {
        // The idle word was read a tick later than the uptime word.
        assert_eq!(cpu_percent(sample(0, 0), sample(10, 11)), 0);
        // A snapshot from before a counter reset cannot go negative either.
        assert_eq!(cpu_percent(sample(0, 5), sample(10, 3)), 0);
    }

    #[test]
    fn a_snapshot_samples_the_idle_counter_not_the_task_rows() {
        let mut snapshot = decode_words(&{
            let mut words = [0u64; WORDS];
            words[header::VERSION] = VERSION;
            words[header::TICKS] = 1234;
            words[header::IDLE_TICKS] = 1000;
            words
        })
        .expect("decodes");
        snapshot.tasks[0] = TaskRow {
            present: true,
            state: TaskState::Runnable,
            cpu_ticks: 1234,
            ..TaskRow::EMPTY
        };
        assert_eq!(snapshot.cpu_sample(), sample(1234, 1000));
    }
}
