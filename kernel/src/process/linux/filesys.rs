//! Syscalls that act on a file's size or on the filesystem holding it rather
//! than on the bytes: `truncate`/`ftruncate`, `fsync`/`fdatasync`/`syncfs`/
//! `sync`, and `statfs`/`fstatfs`.
//!
//! `fsync` flushes the one mount holding the file (`fs::abi_flush`), `sync`
//! every mount, as the durability notes in `docs/architecture/filesystem.md`
//! describe; `fdatasync` is the same call because ext2 has no cheaper form.

use crate::fs::vfs::{FsError, Id, StatFs};
use crate::task::{self, FdKind};
use crate::user_ptr;

use super::cwd::{user_path, AT_FDCWD};
use super::errno::{err, fs_err, EBADF, EFAULT, EINVAL, ENOMEM};
use super::fd::fd_meta_get;
use super::path::synthetic_meta;
use super::vfsfd;

/// `truncate(path, length)`: works on any mount; the VFS checks write access.
pub(super) fn sys_truncate(path: u64, length: u64) -> u64 {
    let path = match user_path(AT_FDCWD, path) {
        Ok(path) => path,
        Err(code) => return code,
    };
    if (length as i64) < 0 {
        return err(EINVAL);
    }
    match crate::fs::abi_truncate(Id::current(), &path, length) {
        Ok(()) => 0,
        Err(error) => fs_err(error),
    }
}

/// `ftruncate(fd, length)`: the descriptor must be open for writing.
pub(super) fn sys_ftruncate(fd: u64, length: u64) -> u64 {
    if (length as i64) < 0 {
        return err(EINVAL);
    }
    match task::fd_kind(fd as usize) {
        FdKind::Vfs => vfsfd::with_file(fd, |file| {
            if !file.writable() {
                return err(EINVAL);
            }
            match file.truncate(length) {
                Ok(()) => 0,
                Err(error) => fs_err(error),
            }
        }),
        FdKind::File => truncate_snapshot(fd, length),
        FdKind::Closed => err(EBADF),
        _ => err(EINVAL), // not a regular file
    }
}

/// `ftruncate` on a snapshot descriptor: cut the backing file, then the
/// snapshot, so the descriptor reads back what it wrote.
fn truncate_snapshot(fd: u64, length: u64) -> u64 {
    let Some(meta) = fd_meta_get(fd as usize) else {
        return err(EBADF);
    };
    let (Some(path), true) = (meta.path, meta.writable) else {
        return err(EINVAL);
    };
    let Ok(length) = usize::try_from(length) else {
        return err(EINVAL);
    };
    // Reserve first: once the file is cut, mirroring must not fail.
    if !task::prepare_fd_write(fd as usize, length, 0) {
        return err(ENOMEM);
    }
    match crate::fs::abi_truncate(Id::current(), &path, length as u64) {
        Ok(()) => {
            task::fd_set_len(fd as usize, length);
            0
        }
        Err(error) => fs_err(error),
    }
}

/// `fsync(fd)` and `fdatasync(fd)`: flush the mount holding the file. A
/// descriptor with nothing to flush (a pipe, a socket, the terminal) is
/// `-EINVAL`, as on Linux.
pub(super) fn sys_fsync(fd: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::Vfs => vfsfd::with_file(fd, |file| status(file.flush())),
        FdKind::File => match fd_meta_get(fd as usize).and_then(|meta| meta.path) {
            Some(path) => status(crate::fs::abi_flush(Id::current(), &path)),
            None => err(EINVAL),
        },
        FdKind::Closed => err(EBADF),
        _ => err(EINVAL),
    }
}

/// `syncfs(fd)`: flush the filesystem `fd` lives on. Any open descriptor names
/// one; those with no file of their own (a pipe) flush everything.
pub(super) fn sys_syncfs(fd: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::Closed => err(EBADF),
        FdKind::Vfs | FdKind::File => sys_fsync(fd),
        _ => sys_sync(),
    }
}

/// `sync()`: flush every mounted filesystem. It cannot report failure, so one
/// is logged instead.
pub(super) fn sys_sync() -> u64 {
    if let Err(error) = crate::fs::abi_sync_all() {
        crate::serial_println!("sync: {}", error.message());
    }
    0
}

/// `statfs(path, buf)`.
pub(super) fn sys_statfs(path: u64, buf: u64) -> u64 {
    let path = match user_path(AT_FDCWD, path) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let id = Id::current();
    let found = match crate::fs::abi_statfs(id, &path) {
        // A fabricated directory (`/bin`, `/proc`, ...) lives on the root.
        Err(FsError::NotFound) if synthetic_meta(&path).is_some() => crate::fs::abi_statfs(id, "/"),
        other => other,
    };
    match found {
        Ok(stat) => write_statfs(buf, &stat),
        Err(error) => fs_err(error),
    }
}

/// `fstatfs(fd, buf)`: for files and directories; other descriptors have no
/// filesystem of their own to describe.
pub(super) fn sys_fstatfs(fd: u64, buf: u64) -> u64 {
    let found = match task::fd_kind(fd as usize) {
        FdKind::Vfs => return vfsfd::with_file(fd, |file| statfs_reply(file.statfs(), buf)),
        FdKind::File => match fd_meta_get(fd as usize).and_then(|meta| meta.path) {
            Some(path) => crate::fs::abi_statfs(Id::current(), &path),
            None => return err(EINVAL),
        },
        FdKind::Closed => return err(EBADF),
        _ => return err(EINVAL),
    };
    statfs_reply(found, buf)
}

fn statfs_reply(found: Result<StatFs, FsError>, buf: u64) -> u64 {
    match found {
        Ok(stat) => write_statfs(buf, &stat),
        Err(error) => fs_err(error),
    }
}

/// Map a flush result to a syscall return.
fn status(result: Result<(), FsError>) -> u64 {
    match result {
        Ok(()) => 0,
        Err(error) => fs_err(error),
    }
}

/// Size of the x86_64 `struct statfs`.
const STATFS_SIZE: usize = 120;

/// Write `stat` as the x86_64 `struct statfs`: `f_type`, `f_bsize`,
/// `f_blocks`, `f_bfree`, `f_bavail`, `f_files`, `f_ffree`, `f_fsid`,
/// `f_namelen`, `f_frsize`, `f_flags`, then four spare words.
fn write_statfs(buf: u64, stat: &StatFs) -> u64 {
    let mut out = [0u8; STATFS_SIZE];
    let words: [u64; 7] = [
        u64::from(stat.magic),
        u64::from(stat.block_size),
        stat.blocks,
        stat.blocks_free,
        stat.blocks_free, // f_bavail: nothing is reserved for root
        stat.files,
        stat.files_free,
    ];
    for (index, word) in words.iter().enumerate() {
        out[index * 8..index * 8 + 8].copy_from_slice(&word.to_le_bytes());
    }
    // f_fsid (offset 56) stays zero; f_namelen and f_frsize follow it.
    out[64..72].copy_from_slice(&u64::from(stat.name_max).to_le_bytes());
    out[72..80].copy_from_slice(&u64::from(stat.block_size).to_le_bytes());
    match user_ptr::try_copy_to(buf, &out) {
        Ok(()) => 0,
        Err(_) => err(EFAULT),
    }
}
