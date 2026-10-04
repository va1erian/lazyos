//! Typed, validated access to user-space addresses.
//!
//! Syscall handlers across `process::linux`, `process`, `display`, `sysinfo`
//! and `ipc::syscalls` receive a `u64` address from ring 3 and read or write a
//! primitive there (or at a byte offset from it). The address is attacker
//! controlled: the `int 0x80`/`syscall` gate runs on the caller's page table
//! with the kernel half mapped, so an unchecked read or write through a
//! user-supplied pointer is an arbitrary kernel read/write primitive.
//!
//! Every access in this module therefore goes through the same page-table
//! walk `ipc::syscalls::{copy_in, copy_out}` use ([`access_range`]): the range
//! must lie in the canonical lower half, every page must be present and
//! `USER` (and writable for a write, privatising a COW page first), and
//! not-present pages inside an `Anon`/`Heap` VMA are demand-materialised as a
//! page fault would.
//!
//! Two API families sit on that check:
//!
//! * the **fallible** `try_*` functions return [`Fault`] for a bad range. Every
//!   native syscall uses these and turns a [`Fault`] into `-EFAULT`. The
//!   exception is [`try_cstr`], which returns [`CStrError`] so a path that is
//!   merely unterminated/too long can be reported as `ENAMETOOLONG`.
//! * the **legacy infallible** primitives ([`read`], [`write`], [`bytes`],
//!   [`copy_to`], ...) keep their old signatures for the Linux ABI shim, whose
//!   call sites have no error path. They validate too, and degrade to a safe
//!   value on a bad range: a read yields zero, a write is dropped and a byte
//!   view is empty. That closes the memory-safety hole for those sites; turning
//!   each one into a proper `-EFAULT` is the follow-up tracked in the issue that
//!   introduced the validation.
//!
//! The in-kernel test suite passes kernel-stack and heap buffers to the
//! syscall surface, which a real user pointer never is. Under
//! `cfg(lazyos_tests)` validation is therefore off by default
//! ([`set_trust_kernel_pointers`]) and the tests that exercise the checks turn
//! it on.

use alloc::vec::Vec;
use core::mem::size_of;

/// A user range that is not mapped, not user-accessible, not writable when a
/// write was requested, or outside the canonical lower half.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fault;

/// Why [`try_cstr`] could not produce a string: the two causes need different
/// errno codes (a path that is merely too long is `ENAMETOOLONG`, unreadable
/// memory is `EFAULT`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CStrError {
    /// The bytes could not be read from user memory.
    Fault,
    /// No NUL terminator appeared within the allowed length.
    Unterminated,
}

/// Plain-old-data integers a user buffer may hold. Sealed: every implementor
/// is valid for any bit pattern, so a byte copy from user memory can never
/// forge an invalid value, and [`Pod::ZERO`] is the degraded read result.
pub trait Pod: Copy + sealed::Sealed {
    /// The all-zero value.
    const ZERO: Self;
}

mod sealed {
    pub trait Sealed {}
}

macro_rules! pod {
    ($($t:ty),*) => {$(
        impl sealed::Sealed for $t {}
        impl Pod for $t { const ZERO: Self = 0; }
    )*};
}
pod!(u8, u16, u32, u64, i32, i64);

/// Whether the harness disables validation so tests can hand the syscall
/// surface kernel buffers (see the module docs).
#[cfg(lazyos_tests)]
static TRUST_KERNEL_POINTERS: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(true);

/// Test-harness switch: `true` skips validation (kernel buffers pass as user
/// pointers), `false` enforces it. Returns the previous setting.
#[cfg(lazyos_tests)]
pub fn set_trust_kernel_pointers(trust: bool) -> bool {
    TRUST_KERNEL_POINTERS.swap(trust, core::sync::atomic::Ordering::Relaxed)
}

/// Validate `[addr, addr + len)` for the calling task.
fn check(addr: u64, len: usize, write: bool) -> Result<(), Fault> {
    #[cfg(lazyos_tests)]
    if TRUST_KERNEL_POINTERS.load(core::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    crate::ipc::syscalls::access_range(addr, len, write).map_err(|_| Fault)
}

/// Read a `T` from `addr`.
pub fn try_read<T: Pod>(addr: u64) -> Result<T, Fault> {
    check(addr, size_of::<T>(), false)?;
    // Safety: `check` proved `addr..addr + size_of::<T>()` is a present, user
    // readable range in the active address space, and `T: Pod` is valid for
    // any bit pattern.
    Ok(unsafe { (addr as *const T).read_unaligned() })
}

/// Write `value` to `addr`.
pub fn try_write<T: Pod>(addr: u64, value: T) -> Result<(), Fault> {
    check(addr, size_of::<T>(), true)?;
    // Safety: `check` proved the range is present, user-accessible and
    // writable (COW already privatised) in the active address space.
    unsafe { (addr as *mut T).write_unaligned(value) };
    Ok(())
}

/// Read the `T` at `addr + index * size_of::<T>()`; `addr` is an array base.
pub fn try_read_at<T: Pod>(addr: u64, index: usize) -> Result<T, Fault> {
    let offset = index.checked_mul(size_of::<T>()).ok_or(Fault)?;
    try_read(addr.checked_add(offset as u64).ok_or(Fault)?)
}

/// Borrow `len` bytes at `addr` as a slice.
///
/// The slice aliases user memory for `'a`; the caller must not hold it across
/// a point where the task's own threads could unmap the range (syscalls run to
/// completion with interrupts off, so within one handler that cannot happen).
pub fn try_bytes<'a>(addr: u64, len: usize) -> Result<&'a [u8], Fault> {
    check(addr, len, false)?;
    if len == 0 {
        return Ok(&[]);
    }
    // Safety: `check` proved the whole range is present and readable in the
    // active address space for the duration of the syscall.
    Ok(unsafe { core::slice::from_raw_parts(addr as *const u8, len) })
}

/// Bytes one copy moves between two poll points (`arch::irq_window`): a
/// megabyte copy is a few hundred microseconds on hardware, far more under
/// emulation, all with interrupts off.
const COPY_PIECE: usize = 64 * 1024;

/// Borrow `len` writable bytes at `addr` as a slice, so a producer can fill
/// user memory in place (the `AF_INET` pump reads a ring straight into it).
///
/// The same rule as [`try_bytes`]: never hold it across a point where the
/// task could block or its threads unmap the range.
pub fn try_bytes_mut<'a>(addr: u64, len: usize) -> Result<&'a mut [u8], Fault> {
    check(addr, len, true)?;
    if len == 0 {
        return Ok(&mut []);
    }
    // SAFETY: `check` proved the whole range is present and writable user
    // memory in the active address space for the duration of the syscall,
    // and nothing in the kernel aliases user pages.
    Ok(unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, len) })
}

/// Copy `src` to `addr`.
pub fn try_copy_to(addr: u64, src: &[u8]) -> Result<(), Fault> {
    check(addr, src.len(), true)?;
    for (index, piece) in src.chunks(COPY_PIECE).enumerate() {
        crate::arch::irq_window::poll_point();
        let at = addr + (index * COPY_PIECE) as u64;
        // Safety: `check` proved `addr..addr + src.len()` is writable user
        // memory in the active address space, and a window never switches
        // tasks, so it stays so; `src` is kernel memory, so they cannot
        // overlap.
        unsafe { core::ptr::copy_nonoverlapping(piece.as_ptr(), at as *mut u8, piece.len()) };
    }
    Ok(())
}

/// Why [`try_read_vec`] produced no buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadVecError {
    /// The user range is not readable (`EFAULT`).
    Fault,
    /// The kernel could not allocate the copy (`ENOMEM`).
    NoMemory,
}

/// Copy `len` bytes from `addr` into a new kernel buffer. `len` is the
/// caller's, so the allocation is fallible rather than an abort.
pub fn try_read_vec(addr: u64, len: usize) -> Result<Vec<u8>, ReadVecError> {
    let source = try_bytes(addr, len).map_err(|_| ReadVecError::Fault)?;
    let mut copy = Vec::new();
    copy.try_reserve_exact(len)
        .map_err(|_| ReadVecError::NoMemory)?;
    for piece in source.chunks(COPY_PIECE) {
        crate::arch::irq_window::poll_point();
        copy.extend_from_slice(piece);
    }
    Ok(copy)
}

/// Copy little-endian `words` to `addr` in one validated write.
pub fn try_copy_words(addr: u64, words: &[u64]) -> Result<(), Fault> {
    let mut bytes = Vec::with_capacity(words.len() * 8);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    try_copy_to(addr, &bytes)
}

/// Read a NUL-terminated string of at most `max` bytes (the NUL is not
/// returned). A string that is not terminated within `max` bytes is
/// [`CStrError::Unterminated`]: callers operate on the returned path, so
/// silently returning a truncated prefix could resolve to an unintended file.
/// Unreadable memory is [`CStrError::Fault`]. The scan validates one page at a
/// time, so a string that ends before an unmapped page is still readable.
pub fn try_cstr(addr: u64, max: usize) -> Result<Vec<u8>, CStrError> {
    let mut out = Vec::new();
    let mut at = addr;
    while out.len() < max {
        let room = 4096 - (at & 0xfff) as usize;
        let take = room.min(max - out.len());
        let chunk = try_bytes(at, take).map_err(|_| CStrError::Fault)?;
        if let Some(end) = chunk.iter().position(|byte| *byte == 0) {
            out.extend_from_slice(&chunk[..end]);
            return Ok(out);
        }
        out.extend_from_slice(chunk);
        at = at.checked_add(take as u64).ok_or(CStrError::Fault)?;
    }
    Err(CStrError::Unterminated)
}

/// Read a `T` from `addr`; zero when the range is invalid.
///
/// # Safety
/// Kept `unsafe` for source compatibility with the pre-validation API; the
/// access itself is validated, so an invalid `addr` degrades to zero rather
/// than touching kernel memory.
#[inline]
pub unsafe fn read<T: Pod>(addr: u64) -> T {
    try_read(addr).unwrap_or(T::ZERO)
}

/// Write `value` to `addr`; dropped when the range is invalid.
///
/// # Safety
/// See [`read`].
#[inline]
pub unsafe fn write<T: Pod>(addr: u64, value: T) {
    let _ = try_write(addr, value);
}

/// Borrow `len` bytes starting at `addr`; empty when the range is invalid.
///
/// # Safety
/// See [`read`]. The slice must not be held past the syscall that produced it.
#[inline]
pub unsafe fn bytes<'a>(addr: u64, len: usize) -> &'a [u8] {
    try_bytes(addr, len).unwrap_or(&[])
}

/// Copy `src` to `addr`; dropped when the range is invalid.
///
/// # Safety
/// See [`read`].
#[inline]
pub unsafe fn copy_to(addr: u64, src: &[u8]) {
    let _ = try_copy_to(addr, src);
}

/// Read the `T` at `addr + index * size_of::<T>()`; zero when invalid.
///
/// # Safety
/// See [`read`].
#[inline]
pub unsafe fn read_at<T: Pod>(addr: u64, index: usize) -> T {
    try_read_at(addr, index).unwrap_or(T::ZERO)
}

/// Read a possibly unaligned `T` from `addr`; zero when invalid.
///
/// # Safety
/// See [`read`].
#[inline]
pub unsafe fn read_unaligned<T: Pod>(addr: u64) -> T {
    try_read(addr).unwrap_or(T::ZERO)
}

/// Write `value` to a possibly unaligned `addr`; dropped when invalid.
///
/// # Safety
/// See [`read`].
#[inline]
pub unsafe fn write_unaligned<T: Pod>(addr: u64, value: T) {
    let _ = try_write(addr, value);
}
