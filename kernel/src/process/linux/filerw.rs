//! Reading and writing regular-file descriptors: the snapshot kind
//! ([`task::Fd::File`], the copy-up root and `/tmp`) and, through
//! [`super::vfsfd`], the VFS-backed kind on the persistent volume. `read` and
//! `write` reach here from [`super::io`]; `pread64`/`pwrite64` are here whole.

use crate::fs::vfs::Id;
use crate::task::{self, FdKind};
use crate::user_ptr;

use super::errno::{err, fs_err, EBADF, EFAULT, EINVAL, ENOMEM, ENOSPC, ESPIPE};
use super::fd::fd_meta_get;
use super::vfsfd;

/// Most bytes one snapshot `write` stages and hands to the ABI VFS at a time,
/// so the kernel-heap copy of a large `write` stays bounded (the VFS-backed
/// path clamps identically, [`super::vfsfd::WRITE_MAX`]).
const WRITE_CHUNK: usize = 1 << 20;

/// Read from a snapshot descriptor into the user buffer at `ptr`.
///
/// The bytes are staged in kernel memory, copied out through the validated
/// path, and the descriptor's offset only advances once the copy succeeded, so
/// a bad buffer is `-EFAULT` and loses nothing.
pub(super) fn read_file_bytes(fd: u64, ptr: u64, len: u64) -> u64 {
    if let Some(device) = device_path(fd) {
        return read_device(&device, ptr, len);
    }
    let Some(chunk) = task::fd_peek(fd as usize, len as usize) else {
        return 0;
    };
    if user_ptr::try_copy_to(ptr, &chunk).is_err() {
        return err(EFAULT);
    }
    task::fd_advance(fd as usize, chunk.len());
    chunk.len() as u64
}

/// Write through a snapshot descriptor: the ABI VFS updates the backing file
/// named by the open file description, then the description's snapshot is
/// patched so it reads back its own writes (from this descriptor, its `dup`s
/// and the copies a `fork` or `execve` inherited). `O_APPEND` descriptors
/// ignore the position and write at the current EOF. `at` is the explicit
/// offset of a `pwrite64` (which also leaves the position alone); `None`
/// writes at the descriptor position.
pub(super) fn write_file(fd: u64, ptr: u64, len: u64, at: Option<u64>) -> u64 {
    if len == 0 {
        return 0;
    }
    let Some(meta) = fd_meta_get(fd as usize) else {
        return err(EBADF);
    };
    if meta.device {
        // /dev/full is always full; the others discard the bytes.
        return if meta.path.as_deref() == Some("/dev/full") {
            err(ENOSPC)
        } else {
            len
        };
    }
    if !meta.writable {
        return err(EBADF);
    }
    let Some(path) = meta.path else {
        return err(EBADF);
    };
    let id = Id::current();
    let position = task::fd_offset(fd as usize).unwrap_or(0);
    // The base offset: EOF for an append, the explicit `pwrite` offset, or the
    // descriptor position. Each piece lands just past the previous one.
    let base = if meta.append {
        match crate::fs::abi_stat(id, &path) {
            Ok(stat) => stat.size,
            Err(error) => return fs_err(error),
        }
    } else {
        at.unwrap_or(position as u64)
    };
    // Staged in pieces: the write may sleep, and the user buffer may be
    // unmapped meanwhile (`vfsfd::stage`). Bounding the staged piece keeps the
    // kernel-heap copy of one `write` from growing with its length; the
    // VFS-backed path clamps the same way (`vfsfd::WRITE_MAX`).
    let mut done = 0u64;
    while done < len {
        let piece = (len - done).min(WRITE_CHUNK as u64) as usize;
        let Some(src) = ptr.checked_add(done) else {
            return if done > 0 { done } else { err(EFAULT) };
        };
        let offset = base.saturating_add(done);
        let bytes = match vfsfd::stage(src, piece) {
            Ok(bytes) => bytes,
            Err(code) => return if done > 0 { done } else { code },
        };
        // Reserve the snapshot's room first: once the backing file has
        // accepted the bytes, mirroring them must not run out of memory.
        if !task::prepare_fd_write(fd as usize, offset as usize, bytes.len()) {
            return if done > 0 { done } else { err(ENOMEM) };
        }
        match crate::fs::abi_write(id, &path, offset, &bytes) {
            Ok(written) => {
                let written_bytes = &bytes[..written];
                // A positional write leaves the shared position alone.
                let mirrored = match at {
                    Some(_) => task::fd_apply_pwrite(fd as usize, offset as usize, written_bytes),
                    None => task::fd_apply_write(fd as usize, offset as usize, written_bytes),
                };
                if !mirrored {
                    return if done > 0 { done } else { err(ENOMEM) };
                }
                done += written as u64;
                if written < bytes.len() {
                    break; // a short write: report the bytes that landed
                }
            }
            Err(error) => return if done > 0 { done } else { fs_err(error) },
        }
    }
    done
}

/// The node a device descriptor was opened as, if `fd` is one.
fn device_path(fd: u64) -> Option<alloc::string::String> {
    fd_meta_get(fd as usize)
        .filter(|meta| meta.device)
        .and_then(|meta| meta.path)
}

/// Most bytes one device read produces (a short read is legal).
const DEVICE_CHUNK: usize = 64 * 1024;

/// A read from a data device: zeros, random bytes, or end-of-file.
fn read_device(device: &str, ptr: u64, len: u64) -> u64 {
    let n = (len as usize).min(DEVICE_CHUNK);
    let mut bytes = alloc::vec![0u8; n];
    match device {
        "/dev/zero" | "/dev/full" => {}
        "/dev/random" | "/dev/urandom" => super::uaccess::fill_random(&mut bytes),
        _ => return 0,
    }
    match user_ptr::try_copy_to(ptr, &bytes) {
        Ok(()) => n as u64,
        Err(_) => err(EFAULT),
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
