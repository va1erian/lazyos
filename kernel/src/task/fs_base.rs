//! The user thread pointer (`%fs` base): validation and the MSR write.
//!
//! Split out of `task/mod.rs` (issue #194): the canonical-address rule is a
//! security boundary (issue #222), and keeping it beside the task table code
//! kept growing an already oversized file.

use super::{current, TASKS};

/// Highest address a task may use as its user thread pointer (`%fs` base): the
/// top of the canonical lower half. `wrmsr IA32_FS_BASE` with any larger value
/// (the non-canonical hole or the higher half) raises #GP in ring 0, so every
/// value that reaches the MSR or a task's saved `fs_base` must pass
/// [`valid_fs_base`].
pub const USER_FS_BASE_MAX: u64 = 0x0000_7fff_ffff_ffff;

/// Whether `value` is a canonical lower-half address, the only kind that is
/// safe as an `%fs` base (issue #222).
pub fn valid_fs_base(value: u64) -> bool {
    value <= USER_FS_BASE_MAX
}

/// Set the current task's user thread pointer (`%fs` base), programming the CPU.
///
/// Returns `false` and changes nothing when `value` is not a valid user `%fs`
/// base (see [`valid_fs_base`]), so a non-canonical `arch_prctl(ARCH_SET_FS)`
/// reports `-EINVAL` instead of faulting on `wrmsr`.
pub fn set_fs_base(value: u64) -> bool {
    if !valid_fs_base(value) {
        return false;
    }
    if let Some(task) = TASKS.lock()[current()].as_mut() {
        task.fs_base = value;
    }
    crate::arch::msr::write(crate::arch::msr::IA32_FS_BASE, value);
    true
}
