//! Run queues (docs/performance-plan.md P6.1): the per-class masks must make
//! exactly the choice the full-table scan made, stay equal to the table under
//! every transition, and keep the stride scheduler's fairness.

use super::*;
use crate::task::{PriorityClass, TaskState, WaitKind};

/// A small deterministic generator (xorshift64*), so a failure replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// Fresh table with the kernel task parked, as the scheduler suite does.
/// Returns the idle-tick count for [`cleanup`] to put back.
fn fresh() -> u64 {
    let idle = task::idle_ticks();
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(true);
    idle
}

/// Finish and reap every task, wake the kernel and reset the table.
///
/// Simulated ticks that land on a blocked task count as idle time
/// (`charge_tick`), but no time passed: restore the counter, or the uptime
/// accounting other suites check would see more idle ticks than ticks.
fn cleanup(idle: u64) {
    task::IDLE_TICKS.store(idle, core::sync::atomic::Ordering::Relaxed);
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in 1..task::MAX_TASKS {
        if task::harness::state(slot).is_some() {
            task::harness::finish(slot, 0);
        }
    }
    while task::reap_child().is_some() {}
    task::set_blocked(false);
    task::harness::reset();
    task::harness::verify_runq();
}

fn children(count: usize) -> Result<Vec<usize>, String> {
    let mut slots = Vec::new();
    for _ in 0..count {
        slots.push(task::spawn_fork().map_err(|error| format!("spawn: {error}"))?);
    }
    Ok(slots)
}

const BLOCKED: TaskState = TaskState::Blocked {
    wait: WaitKind::Sleep,
    deadline: None,
};

/// Random class changes, blocks, wakes, ticks and current-task moves over 48
/// tasks: after every step the run-queue pick equals the full scan's, and the
/// masks equal the table.
pub fn pick_matches_full_scan() -> Result<(), String> {
    let idle = fresh();
    let result = (|| {
        let slots = children(48)?;
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for step in 0..20_000 {
            let slot = slots[rng.below(slots.len())];
            match rng.below(6) {
                0 => {
                    let class = PriorityClass::ALL[rng.below(4)];
                    check!(task::set_priority(slot, class), "set_priority({slot})");
                }
                1 => task::harness::set_state(slot, TaskState::Runnable),
                2 => task::harness::set_state(slot, BLOCKED),
                3 => {
                    task::wake_task(slot);
                }
                4 => {
                    task::harness::simulate_tick();
                }
                _ => task::harness::switch_current(slot),
            }
            let (got, want) = (
                task::harness::next_runnable(),
                task::harness::reference_pick(),
            );
            check!(
                got == want,
                "step {step}: the run queues picked {got}, the full scan {want}"
            );
            if step % 64 == 0 {
                task::harness::verify_runq();
            }
        }
        task::harness::verify_runq();
        Ok(())
    })();
    cleanup(idle);
    result
}

/// Spawn, run, block, finish and reap 3000 tasks in random classes: the done
/// mask must hand every finished task to `wait4` (no zombie left behind), the
/// masks must equal the table throughout, and every slot comes back.
pub fn spawn_exit_soak() -> Result<(), String> {
    let idle = fresh();
    let free_before = task::free_slots();
    let result = (|| {
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        let mut live: Vec<usize> = Vec::new();
        for cycle in 0..3000 {
            if live.len() < 24 {
                let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
                task::set_priority(slot, PriorityClass::ALL[rng.below(4)]);
                live.push(slot);
            }
            let index = rng.below(live.len());
            match rng.below(4) {
                0 => task::harness::set_state(live[index], BLOCKED),
                1 => {
                    task::wake_task(live[index]);
                }
                2 => {
                    task::harness::simulate_tick();
                    task::harness::switch_current(task::KERNEL_TASK);
                }
                _ => {
                    let slot = live.swap_remove(index);
                    task::harness::finish(slot, cycle as u64);
                    let reaped = task::reap_child_slot(slot);
                    check!(
                        reaped == Some(cycle as u64),
                        "cycle {cycle}: slot {slot} reaped as {reaped:?}"
                    );
                }
            }
            if cycle % 16 == 0 {
                task::harness::verify_runq();
            }
        }
        for slot in live.drain(..) {
            task::harness::finish(slot, 0);
            check!(
                task::reap_child_slot(slot).is_some(),
                "slot {slot} was not reapable"
            );
        }
        task::harness::verify_runq();
        check!(
            task::free_slots() == free_before,
            "{} slots free after the soak, {free_before} before",
            task::free_slots()
        );
        Ok(())
    })();
    cleanup(idle);
    result
}

/// The CPU share a weight earns: the inverse of its integer stride
/// (`STRIDE_UNIT / weight` in `task::sched`), scaled to stay integral.
fn rate(weight: u64) -> u128 {
    (1u128 << 40) / (1024 / weight).max(1) as u128
}

/// 32 runnable `Normal` tasks with weights 1..=32 share 32 768 simulated
/// ticks in proportion to their stride rates, within two quanta each: the
/// run queues keep the stride scheduler's fairness bound.
pub fn weighted_share_soak() -> Result<(), String> {
    let idle = fresh();
    let result = (|| {
        let slots = children(32)?;
        let mut total_rate = 0u128;
        for (index, &slot) in slots.iter().enumerate() {
            task::set_priority(slot, PriorityClass::Normal);
            task::set_weight(slot, index as u16 + 1);
            total_rate += rate(index as u64 + 1);
        }
        let ticks = 32_768u64;
        let mut picks = vec![0u64; task::MAX_TASKS];
        for _ in 0..ticks {
            picks[task::harness::simulate_tick()] += 1;
        }
        for (index, &slot) in slots.iter().enumerate() {
            let expected = (ticks as u128 * rate(index as u64 + 1) / total_rate) as u64;
            let got = picks[slot];
            check!(
                got.abs_diff(expected) <= 2,
                "weight {}: {got} ticks, expected {expected}",
                index + 1
            );
        }
        Ok(())
    })();
    cleanup(idle);
    result
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("runq_pick_matches_full_scan", pick_matches_full_scan),
    ("runq_spawn_exit_soak", spawn_exit_soak),
    ("runq_weighted_share_soak", weighted_share_soak),
];
