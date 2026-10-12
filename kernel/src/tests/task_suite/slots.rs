//! The task ceiling (issue #774): kernel stacks that come and go with their
//! tasks, a table that fills to exactly `limit.task_max`, and thread
//! generations that leak neither a slot nor a stack.

use super::*;
use crate::limits::{self, Id};
use crate::task::diag;

/// Spawn threads of `leader` until the table refuses; the slots, and the
/// reason the table gave.
fn fill_threads(leader: usize) -> Result<(Vec<usize>, &'static str), String> {
    let mut slots = Vec::new();
    loop {
        task::harness::switch_current(leader);
        match task::spawn_thread("thread", process::USER_STACK_TOP, 0, 0) {
            Ok(slot) => slots.push(slot),
            Err(reason) => return Ok((slots, reason)),
        }
        check!(
            slots.len() <= task::MAX_TASKS,
            "spawn_thread handed out more slots than the table has"
        );
    }
}

/// End every thread in `slots`, run the tick that switches away from them and
/// reclaim them all.
fn drain_threads(slots: &[usize]) -> Result<(), String> {
    for &slot in slots {
        task::harness::finish(slot, 0);
    }
    if let Some(&first) = slots.first() {
        task::harness::switch_current(first);
        task::harness::simulate_tick();
        task::reclaim_pending();
    }
    for &slot in slots {
        check!(
            task::harness::state(slot).is_none(),
            "slot {slot} was not reclaimed"
        );
    }
    Ok(())
}

/// The ceiling in force: `limit.task_max` slots, the kernel task's included.
fn ceiling() -> usize {
    limits::task_max().min(task::MAX_TASKS)
}

/// A task's kernel stack exists exactly as long as the task: 16-aligned and
/// private to its slot while it lives, zeroed below the first frame, found by
/// address, and gone from the registry once the slot is reclaimed.
pub fn kstack_lives_and_dies_with_its_task() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    let baseline = diag::live_kstacks();
    let used = mem::heap_stats().used;
    check!(
        task::kstack_top(task::KERNEL_TASK) == 0,
        "the kernel task has a task stack"
    );
    let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
    let mut tops = Vec::new();
    for _ in 0..8 {
        task::harness::switch_current(leader);
        let thread = task::spawn_thread("thread", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("thread: {error}"))?;
        let top = task::kstack_top(thread);
        check!(top != 0 && top % 16 == 0, "slot {thread}: top {top:#x}");
        check!(
            diag::kstack_of(top - 8) == Some((thread, top)),
            "slot {thread}: the stack is not found by address"
        );
        check!(
            diag::kstack_of(top).map(|(slot, _)| slot) != Some(thread),
            "slot {thread}: the byte past the stack belongs to it"
        );
        let written = diag::kstack_high_water(thread);
        check!(
            (1..=512).contains(&written),
            "slot {thread}: {written} bytes written before it ever ran"
        );
        tops.push((thread, top));
    }
    for (i, (_, top)) in tops.iter().enumerate() {
        check!(
            tops[i + 1..].iter().all(|(_, other)| other != top),
            "two slots share the stack top {top:#x}"
        );
    }
    check!(
        diag::live_kstacks() == baseline + 9,
        "{} stacks live, expected {}",
        diag::live_kstacks(),
        baseline + 9
    );
    let threads: Vec<usize> = tops.iter().map(|(slot, _)| *slot).collect();
    drain_threads(&threads)?;
    for (slot, top) in &tops {
        check!(
            task::kstack_top(*slot) == 0 && diag::kstack_of(top - 8).is_none(),
            "slot {slot}: the stack survived its task"
        );
    }
    task::harness::reset();
    check!(
        diag::live_kstacks() == baseline,
        "{} stacks left after the reset",
        diag::live_kstacks() - baseline
    );
    check!(
        mem::heap_stats().used <= used + 64 * 1024,
        "the heap grew from {used} to {} across the test",
        mem::heap_stats().used
    );
    Ok(())
}

/// A thread fan-out to the real ceiling: the table refuses with "no free task
/// slot" (not for want of memory) exactly at `limit.task_max`, with one kernel
/// stack per task and none left when they are reaped. Three generations, then
/// `limit.task_max` is lowered and raised again around a live table.
pub fn threads_fill_the_ceiling() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    let baseline = diag::live_kstacks();
    let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
    for generation in 0..3 {
        let (heap, frames) = (mem::heap_stats().used, mem::frame_stats().live());
        let (slots, reason) = fill_threads(leader)?;
        if generation == 0 {
            // The measured price of a task slot: kernel stack and task state.
            crate::trace::test!(
                "task_threads_fill_the_ceiling:INFO:threads={} heap_per_thread={} frames_per_thread={}",
                slots.len(),
                (mem::heap_stats().used - heap) / slots.len().max(1),
                (mem::frame_stats().live() - frames) / slots.len().max(1)
            );
        }
        check!(
            slots.len() == ceiling() - 2,
            "generation {generation}: {} threads, expected {}",
            slots.len(),
            ceiling() - 2
        );
        check!(
            reason == "no free task slot",
            "generation {generation}: the table refused with {reason:?}"
        );
        check!(task::free_slots() == 0, "free slots at the ceiling");
        check!(
            diag::live_kstacks() == baseline + slots.len() + 1,
            "generation {generation}: {} stacks for {} tasks",
            diag::live_kstacks() - baseline,
            slots.len() + 1
        );
        drain_threads(&slots)?;
        check!(
            task::free_slots() == ceiling() - 2,
            "generation {generation}: {} free slots after the drain",
            task::free_slots()
        );
        check!(
            diag::live_kstacks() == baseline + 1,
            "generation {generation}: {} stacks leaked",
            diag::live_kstacks() - baseline - 1
        );
    }

    // Lowering the ceiling under a live table refuses new tasks and keeps the
    // old ones; raising it back lets them in again.
    let (some, _) = {
        limits::set_for_test(Id::TaskMax, 64);
        let result = fill_threads(leader)?;
        limits::reset_for_test();
        result
    };
    check!(some.len() == 62, "{} threads under task_max 64", some.len());
    let (more, _) = fill_threads(leader)?;
    check!(
        more.len() == ceiling() - 2 - some.len(),
        "{} more threads once the ceiling was back",
        more.len()
    );
    drain_threads(&more)?;
    drain_threads(&some)?;
    task::harness::reset();
    Ok(())
}

/// With the heap frozen (`heap_max` at its floor) and most of its free list
/// taken, a flood of threads is refused for want of a kernel stack while the
/// heap still holds its reserve: no failed growth, no allocation that cannot
/// be met, and everything comes back when the threads are reaped.
pub fn threads_stop_before_the_heap_runs_out() -> Result<(), String> {
    const LEFT: usize = 20 << 20;
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
    let failures = mem::heap_stats().grow_failures;
    limits::set_for_test(Id::HeapMax, 16 << 20);
    // Take the free list in 1 MiB pieces (it is fragmented: one big block
    // would not fit) until about LEFT bytes remain.
    let mut ballast: Vec<Vec<u8>> = Vec::new();
    while mem::heap_stats().free > LEFT {
        let mut piece: Vec<u8> = Vec::new();
        if piece.try_reserve_exact(1 << 20).is_err() {
            break;
        }
        piece.resize(1 << 20, 1);
        ballast.push(piece);
    }
    let filled = fill_threads(leader);
    let after = mem::heap_stats();
    drop(ballast);
    limits::reset_for_test();
    let (slots, reason) = filled?;
    drain_threads(&slots)?;
    task::harness::reset();
    check!(
        reason == "out of memory (kernel stack)",
        "{} threads, then {reason:?}",
        slots.len()
    );
    check!(
        after.grow_failures == failures,
        "{} allocations failed to grow the heap",
        after.grow_failures - failures
    );
    check!(
        after.free >= 4 << 20,
        "only {} bytes of heap were left",
        after.free
    );
    check!(
        diag::live_kstacks() == 0,
        "{} stacks left",
        diag::live_kstacks()
    );
    Ok(())
}

/// Soak: 1000 generations of a thread fan-out to a 64-slot ceiling, then
/// reaped together. Every slot and every kernel stack returns each time, the
/// frames and the heap stay put, and the same slots are handed out again.
pub fn thread_ceiling_soak() -> Result<(), String> {
    const GENERATIONS: usize = 1000;
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    limits::set_for_test(Id::TaskMax, 64);
    let outcome = thread_ceiling_generations(GENERATIONS);
    limits::reset_for_test();
    task::harness::reset();
    outcome
}

fn thread_ceiling_generations(generations: usize) -> Result<(), String> {
    let baseline = diag::live_kstacks();
    let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
    let mut steady = None;
    let mut first_slots = Vec::new();
    for generation in 0..generations {
        let (slots, reason) = fill_threads(leader)?;
        check!(
            slots.len() == 62 && reason == "no free task slot",
            "generation {generation}: {} threads ({reason})",
            slots.len()
        );
        if generation == 0 {
            first_slots = slots.clone();
        } else {
            check!(
                slots == first_slots,
                "generation {generation}: the slots were not recycled"
            );
        }
        drain_threads(&slots)?;
        check!(
            diag::live_kstacks() == baseline + 1 && task::free_slots() == 62,
            "generation {generation}: {} stacks, {} free slots",
            diag::live_kstacks() - baseline,
            task::free_slots()
        );
        let now = (mem::frame_stats().live(), mem::heap_stats().used);
        match steady {
            None => steady = Some(now),
            Some((frames, heap)) => {
                check!(
                    now.0 == frames,
                    "generation {generation}: {} frames leaked",
                    now.0.saturating_sub(frames)
                );
                check!(
                    now.1 <= heap + 4096,
                    "generation {generation}: the heap grew from {heap} to {}",
                    now.1
                );
            }
        }
    }
    crate::trace::test!("task_thread_ceiling_soak:INFO:generations={generations} threads=62");
    Ok(())
}
