//! The scheduler's half of the hang report (issue #382).
//!
//! A boot that hangs with interrupts off prints nothing, so the NMI handler
//! (`arch::nmi`) asks this module for what only the scheduler knows: which
//! task the last timer tick interrupted and where, and every task's state and
//! saved context. Everything here is callable from NMI context: no heap, no
//! blocking lock (the task table is only `try_lock`ed), output through the
//! caller's lock-free writer.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU64, Ordering};

use super::{Task, TaskState, CURRENT, TASKS};
use crate::arch::nmi::write_stack;

/// Word indices in a scheduler-saved frame: 15 general registers, then the
/// CPU's interrupt frame.
const RIP: u64 = 15;
const CS: u64 = 16;
const RFLAGS: u64 = 17;
const RSP: u64 = 18;
/// Stack words printed above a task's kernel-mode saved context.
const TASK_STACK_WORDS: usize = 16;

/// The context the most recent timer tick interrupted: tick, slot, rip, cs.
/// Written by every tick (four relaxed stores), read only by the report.
static LAST_TICK: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// Record the context a timer tick interrupted. `rsp` is the frame the tick
/// saved for `slot`.
pub(super) fn note_tick(slot: usize, rsp: u64) {
    // SAFETY: `rsp` is the interrupt frame the scheduler ISR just pushed on
    // the interrupted task's kernel stack; words RIP and CS are inside it.
    let (rip, cs) = unsafe { (frame_word(rsp, RIP), frame_word(rsp, CS)) };
    let tick = crate::arch::idt::TICKS.load(Ordering::Relaxed);
    for (cell, value) in LAST_TICK.iter().zip([tick, slot as u64, rip, cs]) {
        cell.store(value, Ordering::Relaxed);
    }
}

/// Whether the last timer tick interrupted ring-0 code.
#[cfg(lazyos_tests)]
pub(super) fn last_tick_in_kernel() -> bool {
    LAST_TICK[3].load(Ordering::Relaxed) & 3 == 0
}

/// Whether the task table lock is held right now.
pub fn table_locked() -> bool {
    TASKS.is_locked()
}

/// Print the scheduler's view: the last tick's interrupted context and one
/// line per task (plus its kernel stack when it was saved in ring 0).
pub fn write_report(out: &mut impl Write) -> fmt::Result {
    let [tick, slot, rip, cs] = LAST_TICK
        .each_ref()
        .map(|cell| cell.load(Ordering::Relaxed));
    writeln!(
        out,
        "HANG:LASTTICK tick={tick} slot={slot} rip={rip:#x} cs={cs:#x}"
    )?;
    // A held table is itself evidence (a holder preempted or spinning); its
    // contents may be mid-update, so they are not read.
    let Some(tasks) = TASKS.try_lock() else {
        return writeln!(out, "HANG:TASKS table locked; rows unavailable");
    };
    let current = CURRENT.load(Ordering::Relaxed);
    for (slot, task) in tasks.iter().enumerate() {
        if let Some(task) = task {
            write_task(out, slot, task, slot == current)?;
        }
    }
    Ok(())
}

/// One task's line, and its kernel stack when its saved context is ring 0
/// (a task parked inside a syscall, or the kernel mux preempted mid-work).
fn write_task(out: &mut impl Write, slot: usize, task: &Task, current: bool) -> fmt::Result {
    write!(out, "HANG:TASK slot={slot} name={} ", task.name)?;
    match task.state {
        TaskState::Runnable => write!(out, "state=runnable")?,
        TaskState::Done => write!(out, "state=done")?,
        TaskState::Blocked { wait, deadline } => {
            write!(out, "state=blocked({wait:?}, deadline={deadline:?})")?
        }
    }
    write!(
        out,
        " class={:?} pass={} cpu={} wake={:?}",
        task.class, task.pass, task.cpu_ticks, task.wake_reason
    )?;
    if task.rsp == 0 || task.rsp >= u64::MAX - RSP * 8 {
        return writeln!(out, " frame=none");
    }
    // SAFETY: a non-zero `Task::rsp` is an interrupt frame the scheduler
    // saved on the task's kernel stack (or bootstrapped at spawn), mapped in
    // every address space; the running task's frame is stale but still
    // readable memory of that stack.
    let [rip, cs, rflags, rsp] = unsafe { [RIP, CS, RFLAGS, RSP].map(|i| frame_word(task.rsp, i)) };
    writeln!(
        out,
        " frame={:#x} rip={rip:#x} cs={cs:#x} rflags={rflags:#x} rsp={rsp:#x}{}",
        task.rsp,
        if current {
            " (current: frame is stale)"
        } else {
            ""
        }
    )?;
    if cs & 3 == 0 && !current {
        write_stack(out, slot, rsp, TASK_STACK_WORDS)?;
    }
    Ok(())
}

/// Word `index` of the frame at `rsp`.
///
/// # Safety
/// `rsp..rsp + (index + 1) * 8` must be mapped, readable memory.
unsafe fn frame_word(rsp: u64, index: u64) -> u64 {
    core::ptr::read_volatile((rsp + index * 8) as *const u64)
}
