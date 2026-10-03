//! The Linux-ABI side table for open descriptors, and the syscalls that
//! operate on the descriptor table itself rather than on what a descriptor
//! points at: `close`, `lseek`, `dup`/`dup2` and `fcntl`.
//!
//! A `File` descriptor's open file description carries what its open
//! recorded ([`FdMeta`]): `fstat`, `write` and path-relative opens read the
//! mode, inode and backing path from there.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::Meta;
use crate::task::{self, Fd, FdKind};

use super::errno::{err, EBADF, EINVAL, EMFILE, ESPIPE};
use super::flags::{O_NONBLOCK, S_IFCHR};

/// `fcntl` commands and the `dup`/`dup2` descriptor-flag bit this module
/// needs (`O_NONBLOCK` is [`super::flags::O_NONBLOCK`]; that one is shared
/// with the pipe and eventfd families too).
const F_DUPFD: u64 = 0;
const F_GETFD: u64 = 1;
const F_SETFD: u64 = 2;
const F_GETFL: u64 = 3;
const F_SETFL: u64 = 4;
const F_DUPFD_CLOEXEC: u64 = 1030;
const FD_CLOEXEC: u64 = 1;

/// Allocate a descriptor, mapping failure to `-EMFILE`: the table is at
/// `limit.fd_max` (or could not grow), as Linux reports a full table.
pub(super) fn fd_result(slot: Option<usize>) -> u64 {
    match slot {
        Some(fd) => fd as u64,
        None => err(EMFILE),
    }
}

/// What an open recorded about a snapshot file; it travels with the open
/// file description ([`task::SnapFile`]), so `dup`, `fork` and `execve` keep it.
pub(super) type FdMeta = task::FileMeta;

/// What the open behind descriptor `fd` recorded, if it is a snapshot file.
pub(super) fn fd_meta_get(fd: usize) -> Option<FdMeta> {
    task::fd_file_meta(fd)
}

/// Allocate a descriptor for a snapshot (`data`) of the file `meta` describes.
pub(super) fn open_snapshot(data: Vec<u8>, meta: FdMeta) -> u64 {
    fd_result(task::fd_open(Fd::file(data, Some(meta))))
}

/// [`FdMeta`] for a real file or directory open.
pub(super) fn file_meta(meta: Meta, path: String, writable: bool, append: bool) -> FdMeta {
    FdMeta {
        mode: meta.mode as u32,
        ino: meta.ino,
        uid: meta.uid,
        gid: meta.gid,
        path: Some(path),
        writable,
        append,
        device: false,
    }
}

/// Open a synthetic device node (`/dev/null`, `/dev/zero`, `/dev/full`):
/// reads return an empty snapshot, writes are discarded.
pub(super) fn open_device_fd() -> u64 {
    open_snapshot(
        Vec::new(),
        FdMeta {
            mode: S_IFCHR | 0o666,
            ino: 0,
            uid: 0,
            gid: 0,
            path: None,
            writable: true,
            append: false,
            device: true,
        },
    )
}

pub(super) fn sys_close(fd: u64) -> u64 {
    if task::fd_close(fd as usize) {
        0
    } else {
        err(EBADF)
    }
}

pub(super) fn sys_lseek(fd: u64, offset: u64, whence: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::File => match task::fd_seek(fd as usize, offset as i64, whence) {
            Some(pos) => pos,
            None => err(EINVAL),
        },
        FdKind::Vfs => {
            super::vfsfd::with_file(fd, |file| super::vfsfd::seek(file, offset as i64, whence))
        }
        FdKind::Terminal
        | FdKind::Pipe
        | FdKind::Socket
        | FdKind::EventFd
        | FdKind::Epoll
        | FdKind::Listener
        | FdKind::Inet
        | FdKind::Unbound => err(ESPIPE),
        FdKind::Closed => err(EBADF),
    }
}

pub(super) fn sys_dup(nr: u64, a1: u64, a2: u64) -> u64 {
    let slot = if nr == 32 {
        task::fd_dup(a1 as usize)
    } else {
        task::fd_dup2(a1 as usize, a2 as usize)
    };
    match slot {
        Some(fd) => fd as u64,
        None => err(EBADF),
    }
}

/// `fcntl(fd, cmd, arg)`: the descriptor/status flag commands std needs, plus
/// `F_DUPFD`/`F_DUPFD_CLOEXEC` (`O_NONBLOCK` state lives on the shared pipe or
/// socket object, so `dup`/`fork` see the same setting).
pub(super) fn sys_fcntl(fd: u64, cmd: u64, arg: u64) -> u64 {
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => match task::fd_dup_min(fd as usize, arg as usize) {
            Some(new) => {
                if cmd == F_DUPFD_CLOEXEC {
                    task::fd_set_cloexec(new, true);
                }
                new as u64
            }
            None => err(EBADF),
        },
        F_GETFD => match task::fd_kind(fd as usize) {
            FdKind::Closed => err(EBADF),
            _ => task::fd_cloexec(fd as usize) as u64,
        },
        F_SETFD => match task::fd_kind(fd as usize) {
            FdKind::Closed => err(EBADF),
            _ => {
                task::fd_set_cloexec(fd as usize, arg & FD_CLOEXEC != 0);
                0
            }
        },
        F_GETFL => match task::fd_status(fd as usize) {
            Some(flags) => flags,
            None => err(EBADF),
        },
        F_SETFL => match task::fd_set_status(fd as usize, arg & O_NONBLOCK != 0) {
            true => 0,
            false => err(EBADF),
        },
        _ => err(EINVAL),
    }
}

/// Close every `FD_CLOEXEC` descriptor: the `execve` step that drops std's
/// pipe and socket pairs after they have been `dup2`-ed onto 0/1/2. Returns how
/// many were closed (test-visible through `process::linux`).
pub fn close_cloexec_fds() -> usize {
    task::fd_close_cloexec()
}
