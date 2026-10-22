//! Links: `readlink`/`readlinkat`, the hard-link calls `link`/`linkat`, and
//! the symbolic ones `symlink`/`symlinkat`.
//!
//! Hard links live on ext2 and the ramfs (issue #871; the rules are
//! `Vfs::link`'s): a filesystem that cannot hold them (FAT, the copy-up
//! root, a user-space filesystem) answers `EPERM`, as Linux does, and two
//! mounts `EXDEV`. No filesystem stores *symbolic* links, so `symlink` and
//! `symlinkat` answer `EPERM` the same way: `ln -s` then prints "Operation
//! not permitted" rather than "Function not implemented". The only symbolic
//! links that exist are the kernel's own `/proc` ones:
//!
//! * `/proc/self/exe`: the program the caller runs (the file `execve` loaded);
//! * `/proc/self/cwd`: the working directory;
//! * `/proc/self/fd/<n>`: what descriptor `n` refers to — its path for a file
//!   or directory, `/dev/tty` for the terminal, `pipe:[ino]`/`socket:[ino]`/
//!   `anon_inode:[eventfd]` ... for the others.
//!
//! Any other existing path is not a link (`EINVAL`), which is what `realpath`
//! relies on to walk a path component by component; a missing one is `ENOENT`.

use alloc::format;
use alloc::string::String;

use crate::task::{self, Fd};
use crate::user_ptr;

use super::cwd::{read_path, resolve_at, user_path, AT_FDCWD};
use super::errno::{err, fs_err, EFAULT, EINVAL, ENOENT, EPERM};
use super::path::{resolve, self_exe};
use crate::fs::vfs::Id;

/// `readlink(path, buf, size)`.
pub(super) fn sys_readlink(path: u64, buf: u64, size: u64) -> u64 {
    sys_readlinkat(AT_FDCWD, path, buf, size)
}

/// `readlinkat(dirfd, path, buf, size)`.
pub(super) fn sys_readlinkat(dirfd: u64, path: u64, buf: u64, size: u64) -> u64 {
    if (size as i64) <= 0 {
        return err(EINVAL);
    }
    let path = match user_path(dirfd, path) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let target = match link_target(&path) {
        Ok(target) => target,
        Err(code) => return code,
    };
    // Like Linux, a target longer than the buffer is silently truncated and
    // no terminator is written.
    let n = (size as usize).min(target.len());
    match user_ptr::try_copy_to(buf, &target.as_bytes()[..n]) {
        Ok(()) => n as u64,
        Err(_) => err(EFAULT),
    }
}

/// What the link at `path` points to, or the errno for a path that is not one.
pub(super) fn link_target(path: &str) -> Result<String, u64> {
    let path = path.replace("/proc/thread-self/", "/proc/self/");
    match path.as_str() {
        "/proc/self/exe" => return Ok(self_exe()),
        "/proc/self/cwd" => return Ok(task::cwd()),
        _ => {}
    }
    if let Some(number) = path.strip_prefix("/proc/self/fd/") {
        let fd: usize = number.parse().map_err(|_| err(ENOENT))?;
        return fd_target(fd).ok_or_else(|| err(ENOENT));
    }
    match resolve(&path) {
        Ok(_) => Err(err(EINVAL)),
        Err(error) => Err(fs_err(error)),
    }
}

/// The `/proc/self/fd/<fd>` link text for an open descriptor.
pub(super) fn fd_target(fd: usize) -> Option<String> {
    let entry = task::fd_clone(fd)?;
    // A per-descriptor tag stands in for the inode number of the anonymous
    // objects: unique per open object is all a reader can rely on.
    let tag = fd_tag(&entry);
    Some(match &entry {
        Fd::Closed => return None,
        Fd::Terminal => String::from("/dev/tty"),
        Fd::File { .. } => match task::fd_file_meta(fd).and_then(|meta| meta.path) {
            Some(path) => path,
            None => String::from("/dev/null"),
        },
        Fd::Vfs { file } => file.display_path(),
        Fd::Pipe { .. } => format!("pipe:[{tag}]"),
        Fd::Socket { .. } | Fd::UnixListener { .. } | Fd::Unbound { .. } | Fd::Inet { .. } => {
            format!("socket:[{tag}]")
        }
        Fd::Event { .. } => String::from("anon_inode:[eventfd]"),
        Fd::Epoll { .. } => String::from("anon_inode:[eventpoll]"),
        Fd::Endpoint { .. } => String::from("anon_inode:[messenger]"),
        Fd::Memfd { mem } => super::memfd::link_text(mem),
        Fd::Pty { pty, master: false } => format!("/dev/pts/{}", pty.index()),
        Fd::Pty { master: true, .. } => String::from("/dev/ptmx"),
    })
}

/// A stable small number for the shared object behind `entry`.
fn fd_tag(entry: &Fd) -> u64 {
    let address = match entry {
        Fd::Pipe { pipe, .. } => alloc::sync::Arc::as_ptr(pipe) as u64,
        Fd::Socket { pair, .. } => alloc::sync::Arc::as_ptr(pair) as u64,
        Fd::Inet { sock } => alloc::sync::Arc::as_ptr(sock) as u64,
        Fd::UnixListener { listener } => alloc::sync::Arc::as_ptr(listener) as u64,
        _ => 0,
    };
    (address >> 4) & 0xff_ffff
}

/// `symlink(target, path)` / `symlinkat(target, dirfd, path)`: no
/// filesystem here stores symbolic links. The paths are still validated
/// first so a bad pointer is `EFAULT`, as on Linux.
pub(super) fn sys_symlink(target: u64, dirfd: u64, path: u64) -> u64 {
    if user_ptr::try_cstr(target, 4096).is_err() {
        return err(EFAULT);
    }
    match user_path(dirfd, path) {
        Ok(_) => err(EPERM),
        Err(code) => code,
    }
}

/// `linkat`: follow a symbolic link in the old path. There are none (the
/// `/proc` ones name no file a link could be made to), so it changes nothing.
const AT_SYMLINK_FOLLOW: u64 = 0x400;
/// `linkat`: link the file `olddirfd` refers to. Linux allows it only with
/// `CAP_DAC_READ_SEARCH` and answers `ENOENT` without it; here it is
/// answered `ENOENT` always: a descriptor's file is reached by its name, and
/// an unlinked-while-open file (the use the flag exists for, with
/// `O_TMPFILE`) must not get a name back.
const AT_EMPTY_PATH: u64 = 0x1000;

/// `link(old, new)`.
pub(super) fn sys_link(old: u64, new: u64) -> u64 {
    sys_linkat(AT_FDCWD, old, AT_FDCWD, new, 0)
}

/// `linkat(olddirfd, old, newdirfd, new, flags)`: a further name for a
/// regular file.
pub(super) fn sys_linkat(old_dirfd: u64, old: u64, new_dirfd: u64, new: u64, flags: u64) -> u64 {
    if flags & !(AT_SYMLINK_FOLLOW | AT_EMPTY_PATH) != 0 {
        return err(EINVAL);
    }
    let raw = match read_path(old) {
        Ok(raw) => raw,
        Err(code) => return code,
    };
    let new = match user_path(new_dirfd, new) {
        Ok(new) => new,
        Err(code) => return code,
    };
    // An empty old path names nothing, and `AT_EMPTY_PATH` (see above) is
    // refused whatever the path.
    if raw.is_empty() || flags & AT_EMPTY_PATH != 0 {
        return err(ENOENT);
    }
    let old = match resolve_at(old_dirfd, &raw) {
        Ok(old) => old,
        Err(code) => return code,
    };
    match crate::fs::abi_link(Id::current(), &old, &new) {
        Ok(()) => 0,
        Err(error) => fs_err(error),
    }
}
