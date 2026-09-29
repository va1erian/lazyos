//! Attribute syscalls: the `chmod` family (`chmod`, `fchmod`, `fchmodat`), the
//! `chown` family (`chown`, `fchown`, `lchown`, `fchownat`) and the `utime`
//! family (`utime`, `utimes`, `futimesat`, `utimensat`).
//!
//! Linux grew many argument shapes for three operations. This module only
//! decodes them into one [`AttrRequest`] on one node; the POSIX rules (who may
//! change what, setuid clearing, `UTIME_NOW`'s write-permission fallback) live
//! in the VFS (`fs/vfs/setattr.rs`), so every shape gets exactly the same
//! answer.
//!
//! There are no symlinks yet, so `lchown` is `chown` and
//! `AT_SYMLINK_NOFOLLOW` is accepted and changes nothing: every path already
//! names the node itself. Timestamps are whole seconds (the fraction is
//! validated, then dropped: no backend stores it).

use alloc::string::String;

use crate::fs::vfs::{AttrRequest, FsError, Id, Stamp};
use crate::task::{self, FdKind};
use crate::user_ptr;

use super::errno::{err, fs_err, EBADF, EFAULT, EINVAL, ENOENT, EROFS};
use super::fd::fd_meta_get;
use super::path::{resolve_at, synthetic_meta, AT_FDCWD};
use super::uaccess::read_cstr;
use super::vfsfd;

/// `*at` flags this family accepts.
const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const AT_EMPTY_PATH: u64 = 0x1000;
/// `utimensat` nanosecond sentinels.
const UTIME_NOW: i64 = (1 << 30) - 1;
const UTIME_OMIT: i64 = (1 << 30) - 2;
const NSEC_PER_SEC: i64 = 1_000_000_000;
const USEC_PER_SEC: i64 = 1_000_000;

/// The node an attribute syscall names: a path, or an open descriptor.
enum Node {
    Path(String),
    Fd(u64),
}

/// `chmod(path, mode)`.
pub(super) fn sys_chmod(path: u64, mode: u64) -> u64 {
    at(AT_FDCWD, path, 0, mode_request(mode))
}

/// `fchmod(fd, mode)`.
pub(super) fn sys_fchmod(fd: u64, mode: u64) -> u64 {
    apply(Node::Fd(fd), mode_request(mode))
}

/// `fchmodat(dirfd, path, mode)`: the kernel call has no flags argument (libc
/// handles `AT_SYMLINK_NOFOLLOW` itself).
pub(super) fn sys_fchmodat(dirfd: u64, path: u64, mode: u64) -> u64 {
    at(dirfd, path, 0, mode_request(mode))
}

/// `chown(path, uid, gid)`, and `lchown` (no symlinks, see the module docs).
pub(super) fn sys_chown(path: u64, uid: u64, gid: u64) -> u64 {
    at(AT_FDCWD, path, 0, owner_request(uid, gid))
}

/// `fchown(fd, uid, gid)`.
pub(super) fn sys_fchown(fd: u64, uid: u64, gid: u64) -> u64 {
    apply(Node::Fd(fd), owner_request(uid, gid))
}

/// `fchownat(dirfd, path, uid, gid, flags)`.
pub(super) fn sys_fchownat(dirfd: u64, path: u64, uid: u64, gid: u64, flags: u64) -> u64 {
    at(dirfd, path, flags, owner_request(uid, gid))
}

/// `utime(path, times)`: a `struct utimbuf` of two whole-second `time_t`s
/// (`actime`, `modtime`), or `NULL` for "now".
pub(super) fn sys_utime(path: u64, times: u64) -> u64 {
    let request = if times == 0 {
        touch()
    } else {
        match (read_word(times, 0), read_word(times, 1)) {
            (Ok(atime), Ok(mtime)) => explicit(atime, mtime),
            _ => return err(EFAULT),
        }
    };
    at(AT_FDCWD, path, 0, request)
}

/// `utimes(path, times)`: `futimesat` relative to the root.
pub(super) fn sys_utimes(path: u64, times: u64) -> u64 {
    sys_futimesat(AT_FDCWD, path, times)
}

/// `futimesat(dirfd, path, times)`: two `struct timeval`s, or `NULL`.
pub(super) fn sys_futimesat(dirfd: u64, path: u64, times: u64) -> u64 {
    match timeval_request(times) {
        Ok(request) => at(dirfd, path, 0, request),
        Err(code) => code,
    }
}

/// `utimensat(dirfd, path, times, flags)`: two `struct timespec`s (each may be
/// `UTIME_NOW` or `UTIME_OMIT`), or `NULL` for "now". A `NULL` path means
/// `dirfd` itself, which is how libc implements `futimens`.
pub(super) fn sys_utimensat(dirfd: u64, path: u64, times: u64, flags: u64) -> u64 {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return err(EINVAL);
    }
    let request = match timespec_request(times) {
        Ok(request) => request,
        Err(code) => return code,
    };
    if path == 0 {
        if dirfd == AT_FDCWD {
            return err(EFAULT); // no path and no descriptor
        }
        return apply(Node::Fd(dirfd), request);
    }
    at(dirfd, path, flags, request)
}

/// Resolve `(dirfd, path, flags)` and apply `request` there.
fn at(dirfd: u64, path: u64, flags: u64, request: AttrRequest) -> u64 {
    match node_at(dirfd, path, flags) {
        Ok(node) => apply(node, request),
        Err(code) => code,
    }
}

/// The node a `(dirfd, path, flags)` triple names. An empty path is `ENOENT`
/// unless `AT_EMPTY_PATH` asks for `dirfd` itself.
fn node_at(dirfd: u64, path: u64, flags: u64) -> Result<Node, u64> {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return Err(err(EINVAL));
    }
    let path = read_cstr(path).ok_or(err(EFAULT))?;
    if path.is_empty() {
        return match (flags & AT_EMPTY_PATH != 0, dirfd == AT_FDCWD) {
            (false, _) => Err(err(ENOENT)),
            (true, true) => Ok(Node::Path(String::from("/"))), // the cwd
            (true, false) => Ok(Node::Fd(dirfd)),
        };
    }
    resolve_at(dirfd, &path).map(Node::Path).map_err(err)
}

/// Apply `request` to `node` as the calling task; `0` or `-errno`.
fn apply(node: Node, request: AttrRequest) -> u64 {
    let id = Id::current();
    match node {
        Node::Path(path) => on_path(id, &path, request),
        Node::Fd(fd) => on_fd(id, fd, request),
    }
}

fn on_path(id: Id, path: &str, request: AttrRequest) -> u64 {
    match crate::fs::abi_setattr(id, path, request) {
        Ok(_) => 0,
        // A fabricated entry (`/bin`, `/dev`, an applet alias) exists for
        // `stat` but has no node whose attributes could change.
        Err(FsError::NotFound) if synthetic_meta(path).is_some() => err(EROFS),
        Err(error) => fs_err(error),
    }
}

/// The file behind a descriptor, by the path it records. A snapshot
/// descriptor whose name was unlinked has nothing left to change (`ENOENT`);
/// a device node, pipe or socket keeps no attributes here (`EINVAL`).
fn on_fd(id: Id, fd: u64, request: AttrRequest) -> u64 {
    let status = |path: &str| match crate::fs::abi_setattr_open(id, path, request) {
        Ok(_) => 0,
        Err(error) => fs_err(error),
    };
    match task::fd_kind(fd as usize) {
        FdKind::Vfs => vfsfd::with_file(fd, |file| status(&file.path())),
        FdKind::File => match fd_meta_get(fd as usize).and_then(|meta| meta.path) {
            Some(path) => status(&path),
            None => err(EINVAL),
        },
        FdKind::Closed => err(EBADF),
        _ => err(EINVAL),
    }
}

fn mode_request(mode: u64) -> AttrRequest {
    AttrRequest::Mode((mode & 0o7777) as u16)
}

/// `uid_t`/`gid_t` are 32-bit; `-1` leaves the id alone. The register's upper
/// half is not part of the argument.
fn owner_request(uid: u64, gid: u64) -> AttrRequest {
    let id = |raw: u64| Some(raw as u32).filter(|&id| id != u32::MAX);
    AttrRequest::Owner {
        uid: id(uid),
        gid: id(gid),
    }
}

/// Both stamps to "now": the `NULL`-times form of every call in the family.
fn touch() -> AttrRequest {
    AttrRequest::Times {
        atime: Some(Stamp::Now),
        mtime: Some(Stamp::Now),
    }
}

fn explicit(atime: i64, mtime: i64) -> AttrRequest {
    AttrRequest::Times {
        atime: Some(Stamp::At(atime)),
        mtime: Some(Stamp::At(mtime)),
    }
}

/// Decode two `struct timeval { tv_sec, tv_usec }` (or `NULL`).
fn timeval_request(times: u64) -> Result<AttrRequest, u64> {
    if times == 0 {
        return Ok(touch());
    }
    let seconds = |index: usize| -> Result<i64, u64> {
        let usec = read_word(times, index * 2 + 1)?;
        if !(0..USEC_PER_SEC).contains(&usec) {
            return Err(err(EINVAL));
        }
        read_word(times, index * 2)
    };
    Ok(explicit(seconds(0)?, seconds(1)?))
}

/// Decode two `struct timespec { tv_sec, tv_nsec }` (or `NULL`), honouring
/// `UTIME_NOW` and `UTIME_OMIT` in `tv_nsec`.
fn timespec_request(times: u64) -> Result<AttrRequest, u64> {
    if times == 0 {
        return Ok(touch());
    }
    let stamp = |index: usize| -> Result<Option<Stamp>, u64> {
        match read_word(times, index * 2 + 1)? {
            UTIME_NOW => Ok(Some(Stamp::Now)),
            UTIME_OMIT => Ok(None),
            nsec if (0..NSEC_PER_SEC).contains(&nsec) => {
                Ok(Some(Stamp::At(read_word(times, index * 2)?)))
            }
            _ => Err(err(EINVAL)),
        }
    };
    Ok(AttrRequest::Times {
        atime: stamp(0)?,
        mtime: stamp(1)?,
    })
}

/// The `index`-th 64-bit word of a user array, or `-EFAULT`.
fn read_word(base: u64, index: usize) -> Result<i64, u64> {
    user_ptr::try_read_at::<i64>(base, index).map_err(|_| err(EFAULT))
}
