//! The `stat` family: `stat`/`lstat` (both alias to the same handler; LazyOS
//! has no symlinks), `fstat`, and `newfstatat`. All three funnel through
//! [`fill_stat`], which writes the x86_64 `struct stat` layout the ABI needs;
//! a real filesystem node adds its owner and times ([`fill_node_stat`]).

use crate::fs::vfs::{Id, Meta};
use crate::task::{self, FdKind};
use crate::user_ptr;

use super::errno::{err, fs_err, EBADF, EINVAL, ENOENT};
use super::fd::{fd_meta_get, FdMeta};
use super::flags::{S_IFCHR, S_IFIFO, S_IFREG, S_IFSOCK};
use super::path::{resolve, Target};
use super::uaccess::{read_cstr, write_u32, write_u64};

/// Fill a `struct stat` (x86_64 layout) at `buf`.
fn fill_stat(buf: u64, mode: u32, size: u64, ino: u64) {
    if buf == 0 {
        return;
    }
    // Zero the whole struct through the validated path; a bad buffer writes
    // nothing at all (the field writes below are validated too).
    if user_ptr::try_copy_to(buf, &[0u8; 144]).is_err() {
        return;
    }
    write_u64(buf + 8, ino);
    write_u64(buf + 16, 1); // st_nlink
    write_u32(buf + 24, mode); // st_mode
    write_u64(buf + 48, size); // st_size
    write_u64(buf + 56, 4096); // st_blksize
    write_u64(buf + 64, size.div_ceil(512)); // st_blocks
}

/// [`fill_stat`] for a filesystem node: also `st_uid`/`st_gid` and the three
/// timestamps (whole seconds; the `_nsec` fields stay zero).
fn fill_node_stat(buf: u64, meta: &Meta) {
    fill_stat(buf, u32::from(meta.mode), meta.size, meta.ino);
    if buf == 0 {
        return;
    }
    write_u32(buf + 28, meta.uid); // st_uid
    write_u32(buf + 32, meta.gid); // st_gid
    write_u64(buf + 72, meta.times.atime as u64); // st_atime
    write_u64(buf + 88, meta.times.mtime as u64); // st_mtime
    write_u64(buf + 104, meta.times.ctime as u64); // st_ctime
}

/// The current metadata of a snapshot descriptor's file, so `fstat` sees a
/// later `chmod` or write. Only while the path still names the same inode the
/// descriptor opened: after an unlink or a replacing rename, the snapshot's
/// own record is the truth.
fn live_meta(opened: &FdMeta) -> Option<Meta> {
    let path = opened.path.as_deref()?;
    crate::fs::abi_stat(Id::ROOT, path)
        .ok()
        .filter(|meta| meta.ino == opened.ino)
}

pub(super) fn sys_fstat(fd: u64, buf: u64) -> u64 {
    if fd <= 2 {
        fill_stat(buf, S_IFCHR | 0o620, 0, 0);
        return 0;
    }
    match task::fd_kind(fd as usize) {
        FdKind::File => {
            match fd_meta_get(fd as usize) {
                Some(opened) => match live_meta(&opened) {
                    Some(meta) => fill_node_stat(buf, &meta),
                    // The open recorded the VFS mode/ino/size; report those.
                    None => fill_stat(buf, opened.mode, opened.size, opened.ino),
                },
                // Inherited fds (fork/exec) have no side-table entry yet.
                None => {
                    let size = task::fd_size(fd as usize).unwrap_or(0);
                    fill_stat(buf, S_IFREG | 0o444, size, fd);
                }
            }
            0
        }
        FdKind::Vfs => super::vfsfd::with_file(fd, |file| match file.stat() {
            Ok(meta) => {
                fill_node_stat(buf, &meta);
                0
            }
            Err(error) => fs_err(error),
        }),
        FdKind::Terminal => {
            fill_stat(buf, S_IFCHR | 0o620, 0, 0);
            0
        }
        FdKind::Pipe => {
            fill_stat(buf, S_IFIFO | 0o600, 0, fd);
            0
        }
        FdKind::Socket => {
            fill_stat(buf, S_IFSOCK | 0o600, 0, fd);
            0
        }
        FdKind::Listener | FdKind::Unbound => {
            fill_stat(buf, S_IFSOCK | 0o600, 0, fd);
            0
        }
        // eventfd/epoll fds are anonymous inodes; a regular-file mode is the
        // closest the stat ABI gets.
        FdKind::EventFd | FdKind::Epoll => {
            fill_stat(buf, S_IFREG | 0o600, 0, fd);
            0
        }
        FdKind::Closed => err(EBADF),
    }
}

pub(super) fn sys_stat_path(path: u64, buf: u64) -> u64 {
    match read_cstr(path) {
        Some(path) => stat_path(&path, buf),
        None => err(EINVAL),
    }
}

fn stat_path(path: &str, buf: u64) -> u64 {
    match resolve(path) {
        Ok(Target::Node(meta)) | Ok(Target::Synthetic(meta)) => {
            fill_node_stat(buf, &meta);
            0
        }
        Err(error) => fs_err(error),
    }
}

pub(super) fn sys_newfstatat(_dirfd: u64, path: u64, buf: u64, _flags: u64) -> u64 {
    match read_cstr(path) {
        Some(path) if !path.is_empty() => stat_path(&path, buf),
        _ => err(ENOENT),
    }
}
