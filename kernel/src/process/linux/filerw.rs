//! Reading and writing regular-file descriptors: the snapshot kind
//! ([`task::Fd::File`], the copy-up root and `/tmp`) and, through
//! [`super::vfsfd`], the VFS-backed kind on the persistent volume. `read` and
//! `write` reach here from [`super::io`]; `pread64`/`pwrite64` are here whole.

use crate::fs::vfs::Id;
use crate::task::{self, FdKind};
use crate::user_ptr;

use super::errno::{err, fs_err, EBADF, EFAULT, EINVAL, ENOMEM, ESPIPE};
use super::fd::{fd_meta_get, fd_meta_sync_len};
use super::vfsfd;

/// Read from a snapshot descriptor into the user buffer at `ptr`.
///
/// The bytes are staged in kernel memory, copied out through the validated
/// path, and the descriptor's offset only advances once the copy succeeded, so
/// a bad buffer is `-EFAULT` and loses nothing.
pub(super) fn read_file_bytes(fd: u64, ptr: u64, len: u64) -> u64 {
    let Some(chunk) = task::fd_peek(fd as usize, len as usize) else {
        return 0;
    };
    if user_ptr::try_copy_to(ptr, &chunk).is_err() {
        return err(EFAULT);
    }
    task::fd_advance(fd as usize, chunk.len());
    chunk.len() as u64
}

/// Write through a snapshot descriptor: the ABI VFS updates the backing file,
/// then the fd's snapshot is patched so the same descriptor reads back its own
/// writes. `O_APPEND` descriptors ignore the position and write at the current
/// EOF. `at` is the explicit offset of a `pwrite64` (which also leaves the
/// descriptor position alone); `None` writes at the descriptor position.
pub(super) fn write_file(fd: u64, ptr: u64, len: u64, at: Option<u64>) -> u64 {
    if len == 0 {
        return 0;
    }
    let Some(meta) = fd_meta_get(fd as usize) else {
        return err(EBADF);
    };
    if meta.device {
        return len; // /dev/null and friends discard the bytes
    }
    if !meta.writable {
        return err(EBADF);
    }
    let Some(path) = meta.path else {
        return err(EBADF);
    };
    let Ok(bytes) = user_ptr::try_bytes(ptr, len as usize) else {
        return err(EFAULT);
    };
    let id = Id::current();
    let position = task::fd_offset(fd as usize).unwrap_or(0);
    let offset = if meta.append {
        match crate::fs::abi_stat(id, &path) {
            Ok(stat) => stat.size,
            Err(error) => return fs_err(error),
        }
    } else {
        at.unwrap_or(position as u64)
    };
    // Get the descriptor's snapshot ready first: once the backing file has
    // accepted the bytes, mirroring them must not be able to fail.
    if !task::prepare_fd_write(fd as usize, offset as usize, bytes.len()) {
        return err(ENOMEM);
    }
    match crate::fs::abi_write(id, &path, offset, bytes) {
        Ok(written) => {
            if !task::fd_apply_write(fd as usize, offset as usize, &bytes[..written]) {
                return err(ENOMEM);
            }
            if at.is_some() {
                // `fd_apply_write` moved the position past the write; a
                // positional write must not.
                task::fd_seek(fd as usize, position as i64, 0);
            }
            fd_meta_sync_len(fd as usize);
            written as u64
        }
        Err(error) => fs_err(error),
    }
}

/// `pread64(fd, buf, count, offset)`: read without moving the descriptor
/// position. Only seekable descriptors can do it (`-ESPIPE` otherwise).
pub(super) fn sys_pread64(fd: u64, ptr: u64, len: u64, offset: u64) -> u64 {
    if (offset as i64) < 0 {
        return err(EINVAL);
    }
    match task::fd_kind(fd as usize) {
        FdKind::File => {
            let Some(chunk) = task::fd_peek_at(fd as usize, offset as usize, len as usize) else {
                return err(EBADF);
            };
            match user_ptr::try_copy_to(ptr, &chunk) {
                Ok(()) => chunk.len() as u64,
                Err(_) => err(EFAULT),
            }
        }
        FdKind::Vfs => vfsfd::with_file(fd, |file| vfsfd::pread(file, ptr, len, offset)),
        FdKind::Closed => err(EBADF),
        _ => err(ESPIPE),
    }
}

/// `pwrite64(fd, buf, count, offset)`: write without moving the descriptor
/// position.
pub(super) fn sys_pwrite64(fd: u64, ptr: u64, len: u64, offset: u64) -> u64 {
    if (offset as i64) < 0 {
        return err(EINVAL);
    }
    match task::fd_kind(fd as usize) {
        FdKind::File => write_file(fd, ptr, len, Some(offset)),
        FdKind::Vfs => vfsfd::with_file(fd, |file| vfsfd::pwrite(file, ptr, len, offset)),
        FdKind::Closed => err(EBADF),
        _ => err(ESPIPE),
    }
}
