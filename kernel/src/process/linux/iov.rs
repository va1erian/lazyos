//! Vectored I/O: `readv`/`writev` and the positional `preadv`/`pwritev`
//! (plus their `*2` forms). Every one takes a user `iovec` array, so they all
//! share one bounded walk, [`run_vector`], instead of each re-deriving the
//! count and address checks: syscalls run with interrupts off, so an
//! unbounded or wrapping user-supplied array would freeze the machine (#226).

use crate::task::{self, FdKind};
use crate::user_ptr;

use super::errno::{err, EBADF, EFAULT, EINVAL, EOPNOTSUPP, ESPIPE};
use super::filerw::{sys_pread64, sys_pwrite64};
use super::io::{sys_read, sys_write};

/// Most `iovec` entries one vectored call may name (Linux `UIO_MAXIOV`).
const UIO_MAXIOV: u64 = 1024;

/// `preadv2`/`pwritev2` flags that are only scheduling hints, which files
/// that never block can honour by ignoring them (`RWF_HIPRI`, `RWF_NOWAIT`).
/// `RWF_DSYNC`, `RWF_SYNC` and `RWF_APPEND` change what is written or when it
/// is durable, so they are refused rather than silently dropped.
const RWF_IGNORED_HINTS: u64 = 0x1 | 0x8;

/// The `-1` offset that makes `preadv2`/`pwritev2` use the descriptor position.
const OFFSET_CURRENT: u64 = u64::MAX;

/// Read entry `index` of a user `iovec` array as `(base, len)`. A bad array is
/// `-EFAULT` rather than a run of zero-length entries.
fn iovec_at(iov: u64, index: u64) -> Result<(u64, u64), u64> {
    let entry = iov
        .checked_add(index.checked_mul(16).ok_or(err(EFAULT))?)
        .ok_or(err(EFAULT))?;
    let base = user_ptr::try_read::<u64>(entry).map_err(|_| err(EFAULT))?;
    let len_at = entry.checked_add(8).ok_or(err(EFAULT))?;
    let len = user_ptr::try_read::<u64>(len_at).map_err(|_| err(EFAULT))?;
    // A length past `isize::MAX` is `-EINVAL` (as on Linux); it also keeps a
    // byte count distinguishable from an encoded `-errno` below.
    if len > i64::MAX as u64 {
        return Err(err(EINVAL));
    }
    Ok((base, len))
}

/// The result of a vectored transfer that failed with `code` after `total`
/// bytes moved: report the bytes done so a retry cannot repeat them.
pub(super) fn partial_or(total: u64, code: u64) -> u64 {
    if total > 0 {
        total
    } else {
        code
    }
}

/// Add a segment length to a running transfer total; a total that overflows
/// `isize` is `-EINVAL`, as on Linux.
pub(super) fn add_iov_total(total: u64, part: u64) -> Result<u64, u64> {
    match total.checked_add(part) {
        Some(sum) if sum <= i64::MAX as u64 => Ok(sum),
        _ => Err(err(EINVAL)),
    }
}

/// Walk the `count` segments of the user array at `iov`, handing each to
/// `transfer(base, len, done)` (`done` is the bytes moved by earlier
/// segments) and summing the results.
///
/// A result above the segment length is an encoded `-errno`; it is reported
/// as-is only if nothing moved yet. A short transfer ends the walk: the
/// caller retries the rest, and carrying on would put later segments' bytes
/// in the wrong place.
fn run_vector(iov: u64, count: u64, mut transfer: impl FnMut(u64, u64, u64) -> u64) -> u64 {
    if count > UIO_MAXIOV {
        return err(EINVAL);
    }
    let mut total = 0u64;
    for index in 0..count {
        let (base, len) = match iovec_at(iov, index) {
            Ok(entry) => entry,
            Err(code) => return partial_or(total, code),
        };
        let moved = transfer(base, len, total);
        if moved > len {
            return partial_or(total, moved);
        }
        total = match add_iov_total(total, moved) {
            Ok(sum) => sum,
            Err(code) => return code,
        };
        if moved < len {
            break;
        }
    }
    total
}

/// `writev(fd, iov, iovcnt)`: `struct iovec { void *base; size_t len; }`.
pub(super) fn sys_writev(fd: u64, iov: u64, count: u64) -> u64 {
    run_vector(iov, count, |base, len, _| sys_write(fd, base, len))
}

/// `readv(fd, iov, iovcnt)`.
pub(super) fn sys_readv(fd: u64, iov: u64, count: u64) -> u64 {
    run_vector(iov, count, |base, len, _| sys_read(fd, base, len))
}

/// Refuse a positional transfer on anything that cannot seek, in the order
/// Linux does: a closed descriptor is `-EBADF`, a pipe or terminal `-ESPIPE`;
/// then a negative offset is `-EINVAL`.
fn check_positional(fd: u64, offset: u64) -> Result<(), u64> {
    match task::fd_kind(fd as usize) {
        FdKind::File | FdKind::Vfs => {}
        FdKind::Closed => return Err(err(EBADF)),
        _ => return Err(err(ESPIPE)),
    }
    if (offset as i64) < 0 {
        return Err(err(EINVAL));
    }
    Ok(())
}

/// The file position `done` bytes past `offset`. The sum must stay a valid
/// (non-negative) `loff_t`, or the transfer is `-EINVAL`.
fn position(offset: u64, done: u64) -> Result<u64, u64> {
    offset
        .checked_add(done)
        .filter(|at| *at <= i64::MAX as u64)
        .ok_or(err(EINVAL))
}

/// Run one positional call per segment, each at the offset just past the
/// bytes the earlier segments moved.
fn run_positional(iov: u64, count: u64, offset: u64, op: impl Fn(u64, u64, u64) -> u64) -> u64 {
    run_vector(iov, count, |base, len, done| match position(offset, done) {
        Ok(at) => op(base, len, at),
        Err(code) => code,
    })
}

/// `preadv(fd, iov, iovcnt, offset)`: `readv` at `offset`, leaving the
/// descriptor position alone. On x86_64 the offset arrives whole in one
/// register (the kernel's "high half" argument is shifted out), so it is not
/// reassembled here.
pub(super) fn sys_preadv(fd: u64, iov: u64, count: u64, offset: u64) -> u64 {
    if let Err(code) = check_positional(fd, offset) {
        return code;
    }
    run_positional(iov, count, offset, |base, len, at| {
        sys_pread64(fd, base, len, at)
    })
}

/// `pwritev(fd, iov, iovcnt, offset)`.
pub(super) fn sys_pwritev(fd: u64, iov: u64, count: u64, offset: u64) -> u64 {
    if let Err(code) = check_positional(fd, offset) {
        return code;
    }
    run_positional(iov, count, offset, |base, len, at| {
        sys_pwrite64(fd, base, len, at)
    })
}

/// `preadv2(fd, iov, iovcnt, offset, flags)`: `preadv`, or `readv` when
/// `offset` is `-1`.
pub(super) fn sys_preadv2(fd: u64, iov: u64, count: u64, offset: u64, flags: u64) -> u64 {
    if flags & !RWF_IGNORED_HINTS != 0 {
        return err(EOPNOTSUPP);
    }
    if offset == OFFSET_CURRENT {
        return sys_readv(fd, iov, count);
    }
    sys_preadv(fd, iov, count, offset)
}

/// `pwritev2(fd, iov, iovcnt, offset, flags)`: `pwritev`, or `writev` when
/// `offset` is `-1`.
pub(super) fn sys_pwritev2(fd: u64, iov: u64, count: u64, offset: u64, flags: u64) -> u64 {
    if flags & !RWF_IGNORED_HINTS != 0 {
        return err(EOPNOTSUPP);
    }
    if offset == OFFSET_CURRENT {
        return sys_writev(fd, iov, count);
    }
    sys_pwritev(fd, iov, count, offset)
}
