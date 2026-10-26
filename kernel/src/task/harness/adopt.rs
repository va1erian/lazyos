//! Put a suite-made task into a process (the `execve` de-thread tests): a
//! kernel thread (`task::kthread`) runs for real, so making it a thread or
//! the leader of a user process lets the suite drive a wait end to end.

use super::super::TASKS;

/// Make `slot` a member of the process whose address space is `pml4`: its
/// leader when `leader == slot` (then `parent` is the process's parent), a
/// thread of `leader` otherwise (no parent, as `clone(CLONE_THREAD)` gives).
/// The task's own address space, if it had one, is not freed: give this only
/// a kernel thread (which runs on the kernel's table) or a task whose table
/// the caller still owns.
pub fn adopt(slot: usize, pml4: u64, leader: usize, parent: usize) {
    if let Some(task) = TASKS.lock()[slot].as_mut() {
        task.pml4 = pml4;
        task.linux.tgid = if leader == slot { 0 } else { leader };
        task.parent = if leader == slot { parent } else { 0 };
    }
}

/// The leader recorded for `slot` (0 for a leader itself).
pub fn recorded_leader(slot: usize) -> usize {
    TASKS.lock()[slot]
        .as_ref()
        .map_or(0, |task| task.linux.tgid)
}
