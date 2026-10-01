//! The `stat` family: `stat`/`lstat` (both alias to the same handler; LazyOS
//! has no symlinks), `fstat`, and `newfstatat`. They all gather an [`Attrs`]
//! ([`fd_attrs`] or [`path_attrs`], which [`super::statx`] reuses) and write
//! the x86_64 `struct stat` layout the ABI needs with [`fill_stat`].

use crate::fs::vfs::{Id, Meta, Times};
use crate::task::{self, FdKind};
use crate::user_ptr;

use super::cwd::{user_path, AT_FDCWD};
use super::errno::{err, fs_err, EBADF};
use super::fd::{fd_meta_get, FdMeta};
use super::flags::{S_IFCHR, S_IFIFO, S_IFREG, S_IFSOCK};
use super::path::{resolve, Target};
use super::uaccess::{write_u32, write_u64};

/// What the stat-family calls report about a file: type and mode, size, inode,
/// owner and the three timestamps (whole seconds; nodes with no backing file
/// report zero).
pub(super) struct Attrs {
    pub(super) mode: u32,
    pub(super) size: u64,
    pub(super) ino: u64,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) times: Times,
}

impl Attrs {
    /// A node with no recorded owner (a terminal, a pipe, an eventfd): root's.
    fn anonymous(mode: u32, size: u64, ino: u64) -> Attrs {
        Attrs {
            mode,
            size,
            ino,
            uid: 0,
            gid: 0,
            times: Times::default(),
        }
    }

    fn of(meta: &Meta) -> Attrs {
        Attrs {
            mode: u32::from(meta.mode),
            size: meta.size,
            ino: meta.ino,
            uid: meta.uid,
            gid: meta.gid,
            times: meta.times,
        }
    }
}

/// Fill a `struct stat` (x86_64 layout) at `buf`.
fn fill_stat(buf: u64, attrs: &Attrs) {
    if buf == 0 {
        return;
    }
    // Zero the whole struct through the validated path; a bad buffer writes
    // nothing at all (the field writes below are validated too).
    if user_ptr::try_copy_to(buf, &[0u8; 144]).is_err() {
        return;
    }
    write_u64(buf + 8, attrs.ino);
    write_u64(buf + 16, 1); // st_nlink
    write_u32(buf + 24, attrs.mode); // st_mode
    write_u32(buf + 28, attrs.uid); // st_uid
    write_u32(buf + 32, attrs.gid); // st_gid
    write_u64(buf + 48, attrs.size); // st_size
    write_u64(buf + 56, 4096); // st_blksize
    write_u64(buf + 64, attrs.size.div_ceil(512)); // st_blocks
    write_u64(buf + 72, attrs.times.atime as u64); // st_atime (`_nsec` stays zero)
    write_u64(buf + 88, attrs.times.mtime as u64); // st_mtime
    write_u64(buf + 104, attrs.times.ctime as u64); // st_ctime
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

/// The attributes of an open descriptor (`fstat`, and `statx` with
/// `AT_EMPTY_PATH`); `-EBADF` for one that is not open.
pub(super) fn fd_attrs(fd: u64) -> Result<Attrs, u64> {
    if fd <= 2 {
        return Ok(Attrs::anonymous(S_IFCHR | 0o620, 0, 0));
    }
    match task::fd_kind(fd as usize) {
        FdKind::File => Ok(match fd_meta_get(fd as usize) {
            Some(opened) => match live_meta(&opened) {
                Some(meta) => Attrs::of(&meta),
                // The open recorded the VFS mode/ino/size/owner; report those.
                None => Attrs {
                    mode: opened.mode,
                    size: opened.size,
                    ino: opened.ino,
                    uid: opened.uid,
                    gid: opened.gid,
                    times: Times::default(),
                },
            },
            // Inherited fds (fork/exec) have no side-table entry yet.
            None => {
                let size = task::fd_size(fd as usize).unwrap_or(0);
                Attrs::anonymous(S_IFREG | 0o444, size, fd)
            }
        }),
        FdKind::Vfs => super::vfsfd::meta_of(fd).map(|meta| Attrs::of(&meta)),
        FdKind::Terminal => Ok(Attrs::anonymous(S_IFCHR | 0o620, 0, 0)),
        FdKind::Pipe => Ok(Attrs::anonymous(S_IFIFO | 0o600, 0, fd)),
        FdKind::Socket | FdKind::Listener | FdKind::Unbound | FdKind::Inet => {
            Ok(Attrs::anonymous(S_IFSOCK | 0o600, 0, fd))
        }
        // eventfd/epoll fds are anonymous inodes; a regular-file mode is the
        // closest the stat ABI gets.
        FdKind::EventFd | FdKind::Epoll => Ok(Attrs::anonymous(S_IFREG | 0o600, 0, fd)),
        FdKind::Closed => Err(err(EBADF)),
    }
}

/// The attributes of the file at the absolute ABI path `path`.
pub(super) fn path_attrs(path: &str) -> Result<Attrs, u64> {
    match resolve(path) {
        Ok(Target::Node(meta)) | Ok(Target::Synthetic(meta)) => Ok(Attrs::of(&meta)),
        Err(error) => Err(fs_err(error)),
    }
}

/// Write `attrs` as a `struct stat` at `buf`, or pass the error through.
fn reply(attrs: Result<Attrs, u64>, buf: u64) -> u64 {
    match attrs {
        Ok(attrs) => {
            fill_stat(buf, &attrs);
            0
        }
        Err(code) => code,
    }
}

pub(super) fn sys_fstat(fd: u64, buf: u64) -> u64 {
    reply(fd_attrs(fd), buf)
}

pub(super) fn sys_stat_path(path: u64, buf: u64) -> u64 {
    match user_path(AT_FDCWD, path) {
        Ok(path) => reply(path_attrs(&path), buf),
        Err(code) => code,
    }
}

/// `newfstatat(dirfd, path, buf, flags)`: `stat` relative to `dirfd`, or to the
/// descriptor itself for an empty path with `AT_EMPTY_PATH`. Shares its lookup
/// with `statx`, so the two agree on every path form.
pub(super) fn sys_newfstatat(dirfd: u64, path: u64, buf: u64, flags: u64) -> u64 {
    reply(super::statx::lookup(dirfd, path, flags), buf)
}
