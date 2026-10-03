//! Linux errno values and the two helpers every syscall handler in this
//! module tree uses to report failure: [`err`] turns a positive errno into
//! the negative `u64` the ABI returns in `rax`, and [`fs_err`] maps a VFS
//! [`FsError`] onto the errno Linux would give for the same failure.

use crate::fs::vfs::FsError;

// errno values (returned as negative values).
pub(super) const EPERM: u64 = 1;
pub(super) const ESRCH: u64 = 3;
pub(super) const EIO: u64 = 5;
pub(super) const ENXIO: u64 = 6;
pub(super) const E2BIG: u64 = 7;
pub(super) const ENOSYS: u64 = 38;
pub(super) const ENOMEM: u64 = 12;
pub(super) const ERANGE: u64 = 34;
pub(super) const EINVAL: u64 = 22;
pub(super) const ENODEV: u64 = 19;
pub(super) const ENOTTY: u64 = 25;
pub(super) const ENOENT: u64 = 2;
pub(super) const ECHILD: u64 = 10;
pub(super) const EBADF: u64 = 9;
pub(super) const EAGAIN: u64 = 11;
pub(super) const EFAULT: u64 = 14;
pub(super) const ENOEXEC: u64 = 8;
pub(super) const ESPIPE: u64 = 29;
pub(super) const EPIPE: u64 = 32;
pub(super) const EINTR: u64 = 4;
pub(super) const ETIMEDOUT: u64 = 110;
pub(super) const EMFILE: u64 = 24;
pub(super) const ENOTSOCK: u64 = 88;
pub(super) const ENOPROTOOPT: u64 = 92;
pub(super) const EMSGSIZE: u64 = 90;
pub(super) const EAFNOSUPPORT: u64 = 97;
pub(super) const EADDRINUSE: u64 = 98;
pub(super) const ECONNREFUSED: u64 = 111;
pub(super) const ENOTCONN: u64 = 107;
// Filesystem errnos (mapped from `FsError` by `fs_err`).
pub(super) const EACCES: u64 = 13;
pub(super) const EEXIST: u64 = 17;
pub(super) const EOVERFLOW: u64 = 75;
pub(super) const ELOOP: u64 = 40;
pub(super) const ENOTDIR: u64 = 20;
pub(super) const EISDIR: u64 = 21;
pub(super) const ENOSPC: u64 = 28;
pub(super) const EROFS: u64 = 30;
pub(super) const ENAMETOOLONG: u64 = 36;
pub(super) const ENOTEMPTY: u64 = 39;
pub(super) const EOPNOTSUPP: u64 = 95;

pub(super) fn err(e: u64) -> u64 {
    (e as i64).wrapping_neg() as u64
}

/// Map a VFS/filesystem failure to the errno the ABI returns.
pub(super) fn fs_err(error: FsError) -> u64 {
    err(match error {
        FsError::NotFound => ENOENT,
        FsError::Exists => EEXIST,
        FsError::NotDir => ENOTDIR,
        FsError::IsDir => EISDIR,
        FsError::NotEmpty => ENOTEMPTY,
        FsError::Access => EACCES,
        FsError::NotPermitted => EPERM,
        FsError::ReadOnly => EROFS,
        FsError::Invalid => EINVAL,
        FsError::NoSpace => ENOSPC,
        FsError::NameTooLong => ENAMETOOLONG,
        FsError::NotSupported => ENOSYS,
    })
}
