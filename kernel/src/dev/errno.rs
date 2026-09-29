//! `errno` values the `dev_*` syscall returns (issue #240): the same x86_64
//! Linux numbering the Messenger and credential syscalls use, so userspace
//! error handling is uniform.

pub const EPERM: i64 = 1;
pub const ENOENT: i64 = 2;
pub const EIO: i64 = 5;
pub const E2BIG: i64 = 7;
pub const EBADF: i64 = 9;
pub const ENOMEM: i64 = 12;
pub const EACCES: i64 = 13;
pub const EMFILE: i64 = 24;
pub const EFAULT: i64 = 14;
pub const EBUSY: i64 = 16;
pub const ENODEV: i64 = 19;
pub const EINVAL: i64 = 22;
pub const ENOSYS: i64 = 38;
pub const EOVERFLOW: i64 = 75;
pub const ENOTSUP: i64 = 95;
pub const EDQUOT: i64 = 122;

/// A failed `dev_*` operation: a positive errno.
pub type Errno = i64;
