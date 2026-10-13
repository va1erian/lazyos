//! Test hooks over the timer table (the kernel suites).

use super::{with_table, MAX_TIMERS};

/// Test hook: forget every timer.
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    with_table(|table| {
        for index in 0..MAX_TIMERS {
            if table.timers[index].used {
                table.free(index);
            }
        }
    });
}

/// Test hook: the expiry heap's invariants and its agreement with the table.
#[cfg(lazyos_tests)]
pub fn check_for_test() -> Result<(), &'static str> {
    with_table(|table| {
        table.queue.check()?;
        let wall_armed = table
            .timers
            .iter()
            .filter(|t| t.used && t.armed && !t.kind.is_cpu())
            .count();
        if wall_armed != table.queue.len() {
            return Err("armed wall-clock timers and queue entries disagree");
        }
        let cpu_armed = table
            .timers
            .iter()
            .filter(|t| t.used && t.armed && t.kind.is_cpu())
            .count();
        if cpu_armed != table.cpu_armed {
            return Err("armed CPU timers and the counter disagree");
        }
        Ok(())
    })
}
