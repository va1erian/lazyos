//! Links: `readlink`/`readlinkat`, and the calls that would create one
//! (`link`, `linkat`, `symlink`, `symlinkat`).
//!
//! No LazyOS filesystem stores hard or symbolic links, so the creating calls
//! answer `EPERM`, which is what Linux reports for a filesystem that cannot
//! hold them (FAT, for instance): `ln` then prints "Operation not permitted"
//! rather than "Function not implemented". The only links that exist are the
//! kernel's own `/proc` ones:
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

use super::cwd::{user_path, AT_FDCWD};
use super::errno::{err, fs_err, EFAULT, EINVAL, ENOENT, EPERM};
use super::path::{resolve, self_exe};

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
        Fd::Vfs { file } => file.path(),
        Fd::Pipe { .. } => format!("pipe:[{tag}]"),
        Fd::Socket { .. } | Fd::UnixListener { .. } | Fd::Unbound { .. } | Fd::Inet { .. } => {
            format!("socket:[{tag}]")
        }
        Fd::Event { .. } => String::from("anon_inode:[eventfd]"),
        Fd::Epoll { .. } => String::from("anon_inode:[eventpoll]"),
        Fd::Endpoint { .. } => String::from("anon_inode:[messenger]"),
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

/// `link(old, new)` / `linkat(...)` / `symlink(target, path)` /
/// `symlinkat(...)`: no filesystem here stores links. The paths are still
/// validated first so a bad pointer is `EFAULT`, as on Linux.
pub(super) fn sys_make_link(first: u64, second_dirfd: u64, second: u64) -> u64 {
    if user_ptr::try_cstr(first, 4096).is_err() {
        return err(EFAULT);
    }
    match user_path(second_dirfd, second) {
        Ok(_) => err(EPERM),
        Err(code) => code,
    }
}

/// `linkat(olddirfd, old, newdirfd, new, flags)`.
pub(super) fn sys_linkat(old_dirfd: u64, old: u64, new_dirfd: u64, new: u64) -> u64 {
    if let Err(code) = user_path(old_dirfd, old) {
        return code;
    }
    match user_path(new_dirfd, new) {
        Ok(_) => err(EPERM),
        Err(code) => code,
    }
}
