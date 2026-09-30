//! Reclaiming a parentless dead task whose descriptors wake a peer (issue
//! #404).
//!
//! `reclaim_pending` used to drop the dead `Task` (and its fd table) while
//! holding `TASKS`; the last reference to a pipe end notifies the peer's wait
//! queue, which takes `TASKS` again, so the kernel spun forever with
//! interrupts off. The dead task must be dropped after the lock is released
//! and the peer must actually wake.

use super::*;
use crate::ipc::pipe::{self, End, Pipe};
use crate::task::{TaskState, WakeReason};
use alloc::sync::Arc;

/// Fresh table with the kernel task current and runnable, so the tick that
/// switches away from a finished task has somewhere to go.
fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
}

/// A parentless task (forked from init) holding the pipe's only write end.
fn spawn_writer(pipe: &Arc<Pipe>) -> Result<usize, String> {
    task::harness::switch_current(task::KERNEL_TASK);
    let slot = task::spawn_fork().map_err(|error| format!("spawn writer: {error}"))?;
    task::harness::switch_current(slot);
    task::fd_open(task::Fd::pipe_end(Arc::clone(pipe), End::Write)).ok_or("fd_open failed")?;
    Ok(slot)
}

/// Exit `slot`, run the tick that switches away from it (flagging it for
/// reclamation) and reclaim it. Before the fix this call never returned.
fn exit_and_reclaim(slot: usize) -> Result<(), String> {
    task::harness::finish(slot, 0);
    task::harness::switch_current(slot);
    let next = task::harness::simulate_tick();
    check!(next != slot, "the finished task was selected again");
    task::reclaim_pending();
    check!(
        task::harness::state(slot).is_none(),
        "slot {slot} still holds the dead task"
    );
    Ok(())
}

/// A reader parked on the pipe is woken (EOF) when the writer's slot is
/// reclaimed, and the reclamation itself completes.
pub fn reclaim_wakes_pipe_peer() -> Result<(), String> {
    fresh();
    let pipe = Pipe::new().ok_or("Pipe::new failed")?;
    pipe.acquire(End::Read);
    let reader = task::spawn_fork().map_err(|error| format!("spawn reader: {error}"))?;
    let writer = spawn_writer(&pipe)?;
    check!(
        pipe.writers() == 1,
        "the writer's fd did not take the write end"
    );
    pipe.park_reader(reader);
    check!(
        matches!(
            task::harness::state(reader),
            Some(TaskState::Blocked { .. })
        ),
        "the reader is not parked"
    );

    exit_and_reclaim(writer)?;

    check!(
        pipe.writers() == 0,
        "reclaiming the writer left its write end open"
    );
    check!(
        task::harness::state(reader) == Some(TaskState::Runnable),
        "the parked reader was not woken by the reclaimed writer's close: {:?}",
        task::harness::state(reader)
    );
    check!(
        task::harness::take_wake_reason(reader) == Some(WakeReason::Woken),
        "the reader's wake was not recorded"
    );
    check!(
        pipe.poll(End::Read, pipe::POLLIN) & pipe::POLLHUP != 0,
        "the read end does not report EOF after the last writer went"
    );
    pipe.release(End::Read);
    exit_and_reclaim(reader)?;
    drop(pipe);
    check!(Pipe::live() == 0, "the pipe was not freed");
    task::harness::reset();
    Ok(())
}

/// Soak: many writer generations die holding the last write end while a
/// reader is parked. Every round must wake the reader, recycle the slot and
/// return the frames.
pub fn reclaim_pipe_close_soak() -> Result<(), String> {
    const ROUNDS: usize = 512;
    fresh();
    let pipe = Pipe::new().ok_or("Pipe::new failed")?;
    pipe.acquire(End::Read);
    let reader = task::spawn_fork().map_err(|error| format!("spawn reader: {error}"))?;
    let baseline = mem::frame_stats().live();
    let mut slots = Vec::new();
    for round in 0..ROUNDS {
        let writer = spawn_writer(&pipe).map_err(|error| format!("round {round}: {error}"))?;
        slots.push(writer);
        pipe.park_reader(reader);
        exit_and_reclaim(writer).map_err(|error| format!("round {round}: {error}"))?;
        check!(
            task::harness::state(reader) == Some(TaskState::Runnable)
                && task::harness::take_wake_reason(reader) == Some(WakeReason::Woken),
            "round {round}: the reader was not woken"
        );
        check!(pipe.writers() == 0, "round {round}: a write end leaked");
        let live = mem::frame_stats().live();
        check!(
            live == baseline,
            "round {round}: {} frames leaked",
            live.saturating_sub(baseline)
        );
    }
    check!(
        slots.iter().all(|&slot| slot == slots[0]),
        "the writer slot was not recycled: {slots:?}"
    );
    pipe.release(End::Read);
    exit_and_reclaim(reader)?;
    drop(pipe);
    check!(Pipe::live() == 0, "the pipe was not freed");
    serial_println!("TEST:task_reclaim_pipe_close_soak:INFO:rounds={ROUNDS}");
    task::harness::reset();
    Ok(())
}
