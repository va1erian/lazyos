//! The errno values native syscalls return, as positive numbers (a syscall
//! returns `-EPERM`). Linux numbering throughout: the kernel's
//! `ipc::syscalls::errno`, its filesystem and device surfaces, and the Linux
//! ABI all share one table.
//!
//! This is the one definition userspace has: `lazyos_sys::errno` re-exports
//! it and [`crate::client::Errno`] names its entries. It lives here, in the
//! crate with no dependencies, because `lazyos-sys` depends on `libmessenger`
//! and a cycle the other way round is not possible.

/// Not permitted (a capability or identity check failed).
pub const EPERM: i64 = 1;
/// No such file, service, name or entry.
pub const ENOENT: i64 = 2;
/// No such task.
pub const ESRCH: i64 = 3;
/// A signal interrupted the call.
pub const EINTR: i64 = 4;
/// Input/output error.
pub const EIO: i64 = 5;
/// A buffer or argument block is too large (or a reply too small to hold).
pub const E2BIG: i64 = 7;
/// Bad descriptor.
pub const EBADF: i64 = 9;
/// Try again later.
pub const EAGAIN: i64 = 11;
/// Out of memory or quota.
pub const ENOMEM: i64 = 12;
/// Permission denied.
pub const EACCES: i64 = 13;
/// A pointer argument is outside the caller's address space.
pub const EFAULT: i64 = 14;
/// Busy (a resource another task holds).
pub const EBUSY: i64 = 16;
/// Already exists (a registered name, a file).
pub const EEXIST: i64 = 17;
/// No such device.
pub const ENODEV: i64 = 19;
/// Invalid argument.
pub const EINVAL: i64 = 22;
/// This task's descriptor table is full.
pub const EMFILE: i64 = 24;
/// No space left.
pub const ENOSPC: i64 = 28;
/// The peer endpoint is gone.
pub const EPIPE: i64 = 32;
/// A call would deadlock (a nested call on a busy channel).
pub const EDEADLK: i64 = 35;
/// A path is too long.
pub const ENAMETOOLONG: i64 = 36;
/// Not implemented (and every call on a host without the gate).
pub const ENOSYS: i64 = 38;
/// A malformed message.
pub const EBADMSG: i64 = 74;
/// `recv` discarded the message it took off the queue (too big for the
/// buffer, or its objects did not fit the receiver's table); the endpoint is
/// fine and the next `recv` returns the next message.
pub const EMSGSIZE: i64 = 90;
/// Not supported.
pub const ENOTSUP: i64 = 95;
/// A deadline fired.
pub const ETIMEDOUT: i64 = 110;
/// Quota exceeded.
pub const EDQUOT: i64 = 122;
/// A wait or transaction was canceled.
pub const ECANCELED: i64 = 125;
