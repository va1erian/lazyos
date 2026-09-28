//! Raw access to a task's saved register words on its own kernel stack.
//!
//! The timer/syscall/page-fault entry stubs (`task::switch`, `arch::linux`)
//! push a fixed-layout array of words onto a task's own kernel stack before
//! calling into Rust. `task::signal`'s frame-based signal delivery
//! (`regs_from_frame`/`apply_regs_to_frame`) and its syscall-return register
//! capture (`saved_regs_from_stack`) both need to read and rewrite words at a
//! raw offset into that pushed frame. This module is the one place that does
//! the raw pointer arithmetic and volatile access; `task::signal` builds its
//! typed `UserRegs` view on top of it instead of reaching for
//! `core::ptr::read_volatile`/`write_volatile` itself.

/// Read the 8-byte word at `rsp + index * 8`.
///
/// # Safety
/// `rsp` must point at an interrupt/syscall frame the kernel itself saved
/// (using its own known layout, not a caller-supplied one), with at least
/// `index + 1` words available from `rsp` upward.
#[inline]
pub unsafe fn frame_word(rsp: u64, index: usize) -> u64 {
    core::ptr::read_volatile((rsp + index as u64 * 8) as *const u64)
}

/// Write `value` to the 8-byte word at `rsp + index * 8`.
///
/// # Safety
/// Same contract as [`frame_word`].
#[inline]
pub unsafe fn put_frame_word(rsp: u64, index: usize, value: u64) {
    core::ptr::write_volatile((rsp + index as u64 * 8) as *mut u64, value)
}

/// Read the word at `crate::arch::linux::KERNEL_STACK.offset(slot)`. Negative
/// slots look below the stack top, where the Linux `syscall` entry stub
/// pushes the caller's saved registers before dispatch.
///
/// # Safety
/// Must be called from within the current task's own syscall, before the
/// entry stub's pushed words are popped or overwritten by a nested syscall.
#[inline]
pub unsafe fn kernel_stack_word(slot: isize) -> u64 {
    core::ptr::read_volatile((crate::arch::linux::KERNEL_STACK as *const u64).offset(slot))
}
