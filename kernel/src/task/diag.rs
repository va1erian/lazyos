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

/// Stack words printed above the context the last tick interrupted.
const TICK_STACK_WORDS: usize = 32;

/// The context the most recent timer tick interrupted: tick, slot, rip, cs,
/// rflags, rsp. Written by every tick (six relaxed stores), read only by the
/// report: when the scheduler itself is what hangs, this is the only record
/// of the code the tick preempted.
static LAST_TICK: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];

/// Record the context a timer tick interrupted. `rsp` is the frame the tick
/// saved for `slot`.
pub(super) fn note_tick(slot: usize, rsp: u64) {
    // SAFETY: `rsp` is the interrupt frame the scheduler ISR just pushed on
    // the interrupted task's kernel stack; words RIP..RSP are inside it.
    let [rip, cs, rflags, saved_rsp] =
        unsafe { [RIP, CS, RFLAGS, RSP].map(|i| frame_word(rsp, i)) };
    let tick = crate::arch::idt::TICKS.load(Ordering::Relaxed);
    let record = [tick, slot as u64, rip, cs, rflags, saved_rsp];
    for (cell, value) in LAST_TICK.iter().zip(record) {
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
    let [tick, slot, rip, cs, rflags, rsp] = LAST_TICK
        .each_ref()
        .map(|cell| cell.load(Ordering::Relaxed));
    writeln!(
        out,
        "HANG:LASTTICK tick={tick} slot={slot} rip={rip:#x} cs={cs:#x} rflags={rflags:#x} rsp={rsp:#x}"
    )?;
    // A tick that preempted ring-0 code (IF=1 there) is the prime suspect
    // for a held lock: its stack shows the call chain holding it.
    if tick != 0 && cs & 3 == 0 {
        write_stack(out, slot as usize, rsp, TICK_STACK_WORDS)?;
    }
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

/// The kernel stack holding `addr`, as `(slot, top)`.
pub fn kstack_of(addr: u64) -> Option<(usize, u64)> {
    (0..super::MAX_TASKS).find_map(|slot| {
        let top = super::kstack_top(slot);
        (addr >= top - super::KSTACK_SIZE as u64 && addr < top).then_some((slot, top))
    })
}

/// Bytes of `slot`'s kernel stack that were ever written: the distance from
/// its top to the lowest non-zero word. The stacks are zero at boot and never
/// cleared, so this is an all-time high-water mark; a value equal to the stack
/// size means the stack was used (or overflowed) down to its last word.
pub fn kstack_high_water(slot: usize) -> u64 {
    let top = super::kstack_top(slot);
    let mut addr = top - super::KSTACK_SIZE as u64;
    while addr < top {
        // SAFETY: `addr` stays inside the static `KSTACKS` array.
        if unsafe { core::ptr::read_volatile(addr as *const u64) } != 0 {
            return top - addr;
        }
        addr += 8;
    }
    0
}

/// Stack words printed above a fatal fault's frame (the frame itself is 21
/// words; what sat above it is what a smashed return address came from).
const FAULT_STACK_WORDS: u64 = 128;

/// Print, for a fatal ring-0 fault with its saved frame at `frame`, which
/// kernel stack the frame is on, the high-water marks of that stack and its
/// neighbours (an overflow of slot `n + 1` lands on the top of slot `n`), and
/// the words from the frame towards the stack top. Serial only and without
/// the heap (the faulting code may hold its lock); the kernel halts right
/// after.
pub fn print_kstack_report(frame: u64) {
    let Some((slot, top)) = kstack_of(frame) else {
        crate::serial_println!("kernel: frame {frame:#x} is on no task kernel stack");
        return;
    };
    crate::serial_println!(
        "kernel: frame on kstack slot={slot} top={top:#x} depth={} size={}",
        top - frame,
        super::KSTACK_SIZE
    );
    for neighbour in slot.saturating_sub(1)..=(slot + 1).min(super::MAX_TASKS - 1) {
        crate::serial_println!(
            "kernel: kstack slot={neighbour} high_water={} of {}",
            kstack_high_water(neighbour),
            super::KSTACK_SIZE
        );
    }
    let end = top.min(frame + FAULT_STACK_WORDS * 8);
    let mut addr = frame;
    while addr + 4 * 8 <= end {
        // SAFETY: `[frame, top)` is the live part of this kernel stack.
        let words: [u64; 4] = core::array::from_fn(|index| unsafe {
            core::ptr::read_volatile((addr + index as u64 * 8) as *const u64)
        });
        crate::serial_println!(
            "kernel: stack {addr:#x}: {:#018x} {:#018x} {:#018x} {:#018x}",
            words[0],
            words[1],
            words[2],
            words[3]
        );
        addr += 4 * 8;
    }
    while addr < end {
        // SAFETY: as above; the tail of the range, under four words.
        let word = unsafe { core::ptr::read_volatile(addr as *const u64) };
        crate::serial_println!("kernel: stack {addr:#x}: {word:#018x}");
        addr += 8;
    }
}
