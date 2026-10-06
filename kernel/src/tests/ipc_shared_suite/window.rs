//! The shared-buffer window is each address space's own (issue #497).
//!
//! A buffer is visible only in the tables it was mapped into: a task spawned
//! by a task holding a mapping must not inherit it, and an address space that
//! dies must give its window's page tables back.

use super::*;

const RW: u32 = shared::flags::READ | shared::flags::WRITE;

/// Run as `slot`, on its own page tables.
fn enter(slot: usize) -> Result<PhysAddr, String> {
    let table = task::harness::pml4(slot).ok_or("the task has no address space")?;
    task::harness::switch_current(slot);
    mem::switch_to(PhysAddr::new(table));
    Ok(PhysAddr::new(table))
}

/// Back on the kernel task and its table.
fn leave(kernel: PhysAddr) {
    mem::switch_to(kernel);
    task::harness::switch_current(task::KERNEL_TASK);
}

/// Finish `slot` and reap it as its parent `parent`.
fn kill(slot: usize, parent: usize) -> Result<(), String> {
    task::harness::switch_current(parent);
    task::harness::finish(slot, 0);
    let reaped = task::reap_child_slot(slot).is_some();
    check!(reaped, "task {slot} was not reaped by {parent}");
    Ok(())
}

/// A child spawned while its parent holds a mapping sees nothing at that
/// address, and neither does the kernel's table.
pub fn buffer_window_private_per_address_space() -> Result<(), String> {
    fresh()?;
    let kernel = mem::kernel_table();
    let parent = task::spawn_fork().map_err(String::from)?;
    let parent_table = enter(parent)?;
    let handle = shared::create(4096, RW).map_err(buffer_reason)?;
    let va = shared::map(handle).map_err(buffer_reason)?;
    // SAFETY: the buffer was just mapped writable in the active table.
    unsafe { (va as *mut u64).write_volatile(0x4C41_5A59) };
    check!(
        mem::pte_chain(parent_table, va)[3] != 0,
        "the creator's own table has no leaf for its buffer"
    );
    let child = task::spawn_fork().map_err(String::from)?;
    let child_table = PhysAddr::new(task::harness::pml4(child).ok_or("the child has no table")?);
    check!(
        mem::pte_chain(child_table, va)[0] == 0,
        "a spawned child inherited its parent's shared-buffer window: {:x?}",
        mem::pte_chain(child_table, va)
    );
    check!(
        mem::pte_chain(kernel, va)[3] == 0,
        "the kernel's table sees a task's buffer"
    );
    kill(child, parent)?;
    shared::close(handle).map_err(buffer_reason)?;
    leave(kernel);
    kill(parent, task::KERNEL_TASK)
}

/// Stress: tasks that map buffers and die (holding them, or after closing
/// them) give back every frame, page tables of the window included.
pub fn buffer_window_freed_with_address_space() -> Result<(), String> {
    fresh()?;
    let kernel = mem::kernel_table();
    let mut round = |close_first: bool| -> Result<(), String> {
        let slot = task::spawn_fork().map_err(String::from)?;
        enter(slot)?;
        // Two buffers a gigabyte apart would need two directories; one
        // large and one small exercise more than one table.
        let big = shared::create(64 * 4096, RW).map_err(buffer_reason)?;
        shared::map(big).map_err(buffer_reason)?;
        let small = shared::create(4096, RW).map_err(buffer_reason)?;
        shared::map(small).map_err(buffer_reason)?;
        if close_first {
            shared::close(big).map_err(buffer_reason)?;
            shared::close(small).map_err(buffer_reason)?;
        }
        leave(kernel);
        kill(slot, task::KERNEL_TASK)
    };
    for warm in 0..8 {
        round(warm % 2 == 0)?;
    }
    let live = mem::frame_stats().live();
    for index in 0..1_000 {
        round(index % 3 == 0)?;
    }
    check!(
        mem::frame_stats().live() == live,
        "1000 short-lived tasks with buffers leaked {} frames",
        mem::frame_stats().live() as i64 - live as i64
    );
    Ok(())
}
