//! Ring-0 threads for the in-kernel test suite (test builds only).
//!
//! The suite otherwise drives the scheduler without ever switching tasks
//! (`harness::switch_current`). A kernel thread is a real task: its own slot,
//! kernel stack and saved interrupt frame, entering a Rust function in ring 0
//! on the kernel's address space, so wake, yield and interrupt-return paths
//! can be exercised end to end (docs/performance-plan.md P1).
//!
//! A thread never returns; the test that spawned it parks it on a wait queue
//! it owns, then ends it with `harness::finish` and reaps it as the kernel
//! task (its parent).

use super::*;
use x86_64::instructions::segmentation::{Segment, CS, SS};

/// Words of the initial frame: 15 general registers, then RIP, CS, RFLAGS,
/// RSP and SS, the layout `task::switch` restores.
const FRAME_WORDS: u64 = 20;

/// Spawn a kernel thread in `class` that starts at `entry` with interrupts
/// off (like a syscall body). Returns its slot.
pub fn spawn_kernel_thread(
    name: &'static str,
    entry: extern "C" fn() -> !,
    class: PriorityClass,
) -> Result<usize, &'static str> {
    let mut tasks = TASKS.lock();
    let index = (1..MAX_TASKS)
        .find(|&i| tasks[i].is_none())
        .ok_or("no free task slot")?;
    let top = kstack_top(index);
    let base = top - FRAME_WORDS * 8;
    // SAFETY: `base..top` lies inside slot `index`'s static kernel stack, which
    // no task uses: the slot is free and this table lock is held.
    unsafe {
        let frame = base as *mut u64;
        for word in 0..15 {
            core::ptr::write_volatile(frame.add(word), 0);
        }
        core::ptr::write_volatile(frame.add(15), entry as usize as u64);
        core::ptr::write_volatile(frame.add(16), u64::from(CS::get_reg().0));
        // IF clear: the thread runs like a syscall and naps to take interrupts.
        core::ptr::write_volatile(frame.add(17), 0x2);
        // As after a `call`: rsp + 8 is 16-aligned at the function's entry.
        core::ptr::write_volatile(frame.add(18), top - 8);
        core::ptr::write_volatile(frame.add(19), u64::from(SS::get_reg().0));
    }
    let pass = virtual_now(&tasks);
    credentials::reset_for_task(index);
    fpu::reset(index);
    tasks[index] = Some(Task {
        name,
        kind: Kind::Native,
        pml4: mem::kernel_table().as_u64(),
        kstack_top: top,
        rsp: base,
        state: TaskState::Runnable,
        class,
        weight: class.default_weight(),
        pass,
        cpu_ticks: 0,
        wake_reason: None,
        clear_child_tid: 0,
        parent: KERNEL_TASK,
        pgid: index,
        sid: index,
        exit_status: 0,
        heap_break: 0,
        fs_base: 0,
        fds: FdTable::standard(),
        cwd: None,
        linux: LinuxExtras::default(),
        output: Vec::new(),
        input: VecDeque::new(),
    });
    super::runq::sync(&tasks, index);
    Ok(index)
}
