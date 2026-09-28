//! Typed access to validated user-space addresses.
//!
//! Syscall handlers across `process::linux`, `process`, and `ipc::syscalls`
//! all receive a `u64` address that the syscall ABI promises points into the
//! calling task's own memory, then read or write a primitive there (or at a
//! byte offset from it). Before this module each call site independently
//! cast the address and called `core::ptr::read_volatile`/`write_volatile`/
//! `core::slice::from_raw_parts`, writing its own one-line safety comment
//! that repeated the same "the syscall ABI promises this" contract — three
//! files' worth of hand-rolled casts for the same three operations.
//!
//! This module does **not** validate that `addr` is actually mapped, owned
//! by the calling task, or wide enough for `T` — that is a real gap in
//! today's syscall gate (every syscall handler trusts its pointer
//! arguments), not something this module papers over. What it does is the
//! same thing `arch::io` did for port I/O: collapse the mechanical "cast and
//! touch memory" primitive into one place, so that when address validation
//! against the task's VMA list is added, it has exactly one choke point to
//! instrument instead of ~40 scattered call sites.

use core::mem::size_of;

/// Read a `T` from `addr`.
///
/// # Safety
/// `addr` must be non-null, aligned enough for a volatile access to
/// typically not fault on this target, and point to `size_of::<T>()` bytes
/// of memory the caller may read for the duration of this call (the syscall
/// ABI's promise about its arguments).
#[inline]
pub unsafe fn read<T: Copy>(addr: u64) -> T {
    (addr as *const T).read_volatile()
}

/// Write `value` to `addr`.
///
/// # Safety
/// `addr` must point to `size_of::<T>()` bytes of memory the caller may
/// write for the duration of this call (the syscall ABI's promise about its
/// arguments).
#[inline]
pub unsafe fn write<T: Copy>(addr: u64, value: T) {
    (addr as *mut T).write_volatile(value)
}

/// Borrow `len` bytes starting at `addr` as a byte slice.
///
/// # Safety
/// `addr..addr + len` must be valid, readable memory for the lifetime `'a`
/// the caller assigns to the returned slice, and must not be concurrently
/// written during that lifetime (the syscall ABI's promise about its
/// arguments).
#[inline]
pub unsafe fn bytes<'a>(addr: u64, len: usize) -> &'a [u8] {
    core::slice::from_raw_parts(addr as *const u8, len)
}

/// Copy `src` to `addr`.
///
/// # Safety
/// `addr..addr + src.len()` must be valid, writable memory the caller may
/// write for the duration of this call (the syscall ABI's promise about its
/// arguments), and must not overlap `src`.
#[inline]
pub unsafe fn copy_to(addr: u64, src: &[u8]) {
    core::ptr::copy_nonoverlapping(src.as_ptr(), addr as *mut u8, src.len())
}

/// Read the `T` at `addr + index * size_of::<T>()`, i.e. `addr` treated as
/// the base of a `T` array.
///
/// # Safety
/// Same contract as [`read`], at `addr + index * size_of::<T>()`.
#[inline]
pub unsafe fn read_at<T: Copy>(addr: u64, index: usize) -> T {
    read(addr + (index * size_of::<T>()) as u64)
}

/// Write `value` at `addr + index * size_of::<T>()`, i.e. `addr` treated as
/// the base of a `T` array.
///
/// # Safety
/// Same contract as [`write`], at `addr + index * size_of::<T>()`.
#[inline]
pub unsafe fn write_at<T: Copy>(addr: u64, index: usize, value: T) {
    write(addr + (index * size_of::<T>()) as u64, value)
}
