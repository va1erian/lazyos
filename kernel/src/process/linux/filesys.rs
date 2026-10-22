//! Syscalls that act on a file's size or on the filesystem holding it rather
//! than on the bytes: `truncate`/`ftruncate`, `fallocate`, `fsync`/`fdatasync`/
//! `syncfs`/`sync`, and `statfs`/`fstatfs`.
//!
//! `fsync` flushes the one mount holding the file (`fs::abi_flush`), `sync`
//! every mount, as the durability notes in `docs/architecture/filesystem.md`
//! describe; `fdatasync` is the same call because ext2 has no cheaper form.

use crate::fs::vfs::{FsError, Id, StatFs};
use crate::ipc::pipe::End;
use crate::task::{self, Fd, FdKind};
use crate::user_ptr;

use super::cwd::{user_path, AT_FDCWD};
use super::errno::{err, fs_err, EBADF, EFAULT, EFBIG, EINVAL, ENODEV, ENOMEM, EOPNOTSUPP, ESPIPE};
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
        FdKind::Memfd => super::memfd::with_memfd(fd, |mem| super::memfd::truncate(mem, length)),
        FdKind::Closed => err(EBADF),
        _ => err(EINVAL), // not a regular file
    }
}

/// `fallocate(fd, mode, offset, len)`: the descriptor and the arguments are judged in the order Linux's
/// `vfs_fallocate` does (a closed descriptor `EBADF`, a negative offset or no length `EINVAL`, a descriptor not
/// open for writing `EBADF`, a pipe `ESPIPE`, anything that is not a regular file `ENODEV`, a range past the
/// largest file `EFBIG`), and every valid request is `EOPNOTSUPP`, what Linux answers for ext2 and for the ramfs
/// of `/tmp`. The mode is not looked at: every mode gets that answer, so it could only change which error comes
/// first for a descriptor that is refused anyway.
///
/// Reserving a range means allocating (and, on ext2, zeroing) its blocks, as large a write as the range:
/// the burst WebKit's HTTP cache asks for the call to avoid (`WTF::FileSystem::createMappedFileData`
/// calls it for every body and carries on with the sparse file when the answer is `EOPNOTSUPP`; any other
/// error deletes the file and the cache stores the body inline, issue #864). The price of the honest
/// answer is that a store through a shared mapping of a sparse file can find the volume full at
/// write-back; the page cache keeps what it could not write.
pub(super) fn sys_fallocate(fd: u64, _mode: u64, offset: u64, len: u64) -> u64 {
    let entry = match task::fd_clone(fd as usize) {
        // `fdget` comes before any argument is looked at.
        None | Some(Fd::Closed) => return err(EBADF),
        Some(entry) => entry,
    };
    if (offset as i64) < 0 || (len as i64) <= 0 {
        return err(EINVAL);
    }
    if let Some(errno) = refused_for_allocation(&entry) {
        return err(errno);
    }
    if offset.checked_add(len).is_none_or(|end| (end as i64) < 0) {
        return err(EFBIG);
    }
    err(EOPNOTSUPP)
}

/// Why `fallocate` refuses descriptor `entry` before it looks at the file system, in Linux's order: not open
/// for writing (a pipe's read end too) `EBADF`, a pipe `ESPIPE`, a device or anything else that is not a regular
/// file `ENODEV`.
fn refused_for_allocation(entry: &Fd) -> Option<u64> {
    match entry {
        Fd::Vfs { file } => (!file.writable()).then_some(EBADF),
        Fd::File { file } => match file.meta() {
            Some(meta) if meta.writable && !meta.device => None,
            Some(meta) if meta.writable => Some(ENODEV),
            _ => Some(EBADF),
        },
        Fd::Memfd { .. } => None,
        Fd::Pipe { end: End::Read, .. } => Some(EBADF),
        Fd::Pipe { .. } => Some(ESPIPE),
        _ => Some(ENODEV),
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
            Some(path) => status(crate::fs::abi_flush(Id::current(), &path, false)),
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
        FdKind::Vfs => vfsfd::with_file(fd, |file| status(file.flush_fs())),
        FdKind::File => match fd_meta_get(fd as usize).and_then(|meta| meta.path) {
            Some(path) => status(crate::fs::abi_flush(Id::current(), &path, true)),
            None => err(EINVAL),
        },
        _ => sys_sync(),
    }
}

/// `sync()`: flush every mounted filesystem. It cannot report failure, so one
/// is logged instead.
pub(super) fn sys_sync() -> u64 {
    // Stores through shared file mappings are file data too.
    crate::fs::openfile::writeback_all(true);
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
