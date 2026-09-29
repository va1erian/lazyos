//! The Linux-ABI side table for open descriptors, and the syscalls that
//! operate on the descriptor table itself rather than on what a descriptor
//! points at: `close`, `lseek`, `dup`/`dup2`, `fcntl`, and `getdents64` (a
//! thin wrapper over [`super::io::read_file_bytes`], since a directory's
//! `linux_dirent64` stream is just bytes once [`super::path`] has built it).
//!
//! The task fd table ([`task::Fd`]) carries only bytes for a `File`
//! descriptor, so `fstat`, `write`, and path-relative opens read
//! mode/ino/size and the backing path from [`FdMeta`], keyed by
//! `(task slot, fd)`.

use alloc::string::String;
use alloc::vec::Vec;

use spin::Mutex;

use crate::fs::vfs::Meta;
use crate::task::{self, Fd, FdKind};

use super::errno::{err, EBADF, EINVAL, ENOMEM, ENOTDIR, ESPIPE};
use super::filerw::read_file_bytes;
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

/// Allocate a descriptor, mapping failure to `-ENOMEM`.
pub(super) fn fd_result(slot: Option<usize>) -> u64 {
    match slot {
        Some(fd) => fd as u64,
        None => err(ENOMEM),
    }
}

/// File descriptor metadata captured at open time. The task fd table
/// (`task::Fd::File`) carries only bytes, so `fstat` and `write` read
/// mode/ino/size and the backing path from this side table, keyed by
/// `(task slot, fd)`; `close` clears the slot and `dup` copies it. Fds
/// inherited by `fork` fall back to the plain 0o444 answer until the fd table
/// itself carries VFS handles.
#[derive(Clone)]
pub(super) struct FdMeta {
    pub(super) mode: u32,
    pub(super) ino: u64,
    pub(super) size: u64,
    /// Byte length of the snapshot, checked on read so a slot reused by a
    /// different file (fork-inherited or a recycled task slot) reports no
    /// metadata instead of a stale mode.
    data_len: usize,
    /// Absolute ABI path backing a real file or directory open; `None` for
    /// the synthetic device descriptors.
    pub(super) path: Option<String>,
    /// Whether the descriptor accepts `write(2)` (the open had an access
    /// mode other than `O_RDONLY`).
    pub(super) writable: bool,
    /// `O_APPEND`: writes ignore the descriptor position and land at EOF.
    pub(super) append: bool,
    /// Synthetic nodes (`/dev/null`, `/dev/zero`, `/dev/full`): writes are
    /// discarded and reads return the snapshot (empty).
    pub(super) device: bool,
}

const FD_META_SLOTS: usize = task::MAX_TASKS * task::FD_COUNT;
static FD_META: Mutex<[Option<FdMeta>; FD_META_SLOTS]> =
    Mutex::new([const { None }; FD_META_SLOTS]);

/// This task's side-table slot for `fd`, if both are in range.
fn fd_meta_slot(fd: usize) -> Option<usize> {
    let slot = task::current();
    if fd >= task::FD_COUNT || slot >= task::MAX_TASKS {
        return None;
    }
    Some(slot * task::FD_COUNT + fd)
}

pub(super) fn fd_meta_set(fd: usize, meta: FdMeta) {
    if let Some(slot) = fd_meta_slot(fd) {
        FD_META.lock()[slot] = Some(meta);
    }
}

pub(super) fn fd_meta_get(fd: usize) -> Option<FdMeta> {
    let slot = fd_meta_slot(fd)?;
    let mut table = FD_META.lock();
    let meta = table[slot].clone()?;
    if task::fd_size(fd) != Some(meta.data_len as u64) {
        table[slot] = None; // the slot now holds a different file
        return None;
    }
    Some(meta)
}

pub(super) fn fd_meta_clear(fd: usize) {
    if let Some(slot) = fd_meta_slot(fd) {
        FD_META.lock()[slot] = None;
    }
}

fn fd_meta_copy(from: usize, to: usize) {
    let (Some(from), Some(to)) = (fd_meta_slot(from), fd_meta_slot(to)) else {
        return;
    };
    let mut table = FD_META.lock();
    table[to] = table[from].clone();
}

/// Re-sync the cached snapshot length after a write extended the fd buffer.
pub(super) fn fd_meta_sync_len(fd: usize) {
    let Some(slot) = fd_meta_slot(fd) else {
        return;
    };
    let Some(size) = task::fd_size(fd) else {
        return;
    };
    let mut table = FD_META.lock();
    if let Some(meta) = table[slot].as_mut() {
        meta.data_len = size as usize;
        meta.size = size;
    }
}

/// Allocate a descriptor for a snapshot (`data`) and record its side-table
/// metadata. The snapshot length is patched into `meta` so callers do not
/// repeat it.
pub(super) fn open_snapshot(data: Vec<u8>, mut meta: FdMeta) -> u64 {
    meta.data_len = data.len();
    match task::fd_open(Fd::File {
        data: alloc::sync::Arc::new(data),
        offset: 0,
    }) {
        Some(fd) => {
            fd_meta_set(fd, meta);
            fd as u64
        }
        None => err(ENOMEM),
    }
}

/// [`FdMeta`] for a real file or directory open.
pub(super) fn file_meta(meta: Meta, path: String, writable: bool, append: bool) -> FdMeta {
    FdMeta {
        mode: meta.mode as u32,
        ino: meta.ino,
        size: meta.size,
        data_len: 0,
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
            size: 0,
            data_len: 0,
            path: None,
            writable: true,
            append: false,
            device: true,
        },
    )
}

pub(super) fn sys_getdents64(fd: u64, buf: u64, count: u64) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::File => read_file_bytes(fd, buf, count),
        FdKind::Vfs => err(ENOTDIR), // a regular file has no directory stream
        _ => err(EBADF),
    }
}

pub(super) fn sys_close(fd: u64) -> u64 {
    if task::fd_close(fd as usize) {
        fd_meta_clear(fd as usize);
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
        Some(fd) => {
            fd_meta_copy(a1 as usize, fd);
            fd as u64
        }
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
                fd_meta_copy(fd as usize, new);
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
    // The `(task, fd)` metadata side table is keyed by slot; clear the marked
    // slots first so a closed file's mode/size cannot outlive the exec.
    for fd in 0..task::FD_COUNT {
        if task::fd_cloexec(fd) {
            fd_meta_clear(fd);
        }
    }
    task::fd_close_cloexec()
}
