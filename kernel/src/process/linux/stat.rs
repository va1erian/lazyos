//! The `stat` family: `stat`/`lstat` (both alias to the same handler; LazyOS
//! has no symlinks), `fstat`, and `newfstatat`. All three funnel through
//! [`fill_stat`], which writes the x86_64 `struct stat` layout the ABI needs.

use crate::task::{self, FdKind};
use crate::user_ptr;

use super::errno::{err, fs_err, EBADF, EINVAL, ENOENT};
use super::fd::fd_meta_get;
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

pub(super) fn sys_fstat(fd: u64, buf: u64) -> u64 {
    if fd <= 2 {
        fill_stat(buf, S_IFCHR | 0o620, 0, 0);
        return 0;
    }
    match task::fd_kind(fd as usize) {
        FdKind::File => {
            match fd_meta_get(fd as usize) {
                // The open recorded the VFS mode/ino/size; report those.
                Some(meta) => fill_stat(buf, meta.mode, meta.size, meta.ino),
                // Inherited fds (fork/exec) have no side-table entry yet.
                None => {
                    let size = task::fd_size(fd as usize).unwrap_or(0);
                    fill_stat(buf, S_IFREG | 0o444, size, fd);
                }
            }
            0
        }
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
            fill_stat(buf, meta.mode as u32, meta.size, meta.ino);
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
