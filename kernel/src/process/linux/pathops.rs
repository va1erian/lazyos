//! The naming syscalls that don't open a descriptor: `mkdir`/`mkdirat`,
//! `rmdir`, `unlink`/`unlinkat`, `rename`/`renameat`, `access`, `umask`,
//! and `readlink`. Split out of [`super::path`] (which keeps path
//! resolution and `open`/`openat`) purely to stay under the file size limit;
//! the two are one responsibility and share its helpers freely.

use alloc::string::String;

use crate::fs::vfs::{self, FsError, Id};
use crate::task;
use crate::user_ptr;

use super::cwd::{names_dot, read_path, resolve_at, user_path, AT_FDCWD};
use super::errno::{err, fs_err, EINVAL, ENOENT};
use super::path::synthetic_meta;

/// `unlinkat(2)` flag: remove a directory instead of a file.
const AT_REMOVEDIR: u64 = 0x200;

/// `mkdir(path, mode)`.
pub(super) fn sys_mkdir(path: u64, mode: u64) -> u64 {
    sys_mkdirat(AT_FDCWD, path, mode)
}

/// `mkdirat(dirfd, path, mode)`.
pub(super) fn sys_mkdirat(dirfd: u64, path: u64, mode: u64) -> u64 {
    match user_path(dirfd, path) {
        Ok(path) => mkdir_path(&path, mode),
        Err(code) => code,
    }
}

fn mkdir_path(path: &str, mode: u64) -> u64 {
    let mode = match (mode & 0o7777) as u16 {
        0 => 0o777,
        mode => mode,
    };
    match crate::fs::abi_mkdir(Id::current(), path, mode) {
        Ok(_) => 0,
        Err(error) => fs_err(error),
    }
}

/// `rmdir(path)`.
pub(super) fn sys_rmdir(path: u64) -> u64 {
    unlink_at(AT_FDCWD, path, true)
}

/// `unlink(path)`.
pub(super) fn sys_unlink(path: u64) -> u64 {
    unlink_at(AT_FDCWD, path, false)
}

/// `unlinkat(dirfd, path, flags)`: `AT_REMOVEDIR` selects `rmdir` semantics.
pub(super) fn sys_unlinkat(dirfd: u64, path: u64, flags: u64) -> u64 {
    unlink_at(dirfd, path, flags & AT_REMOVEDIR != 0)
}

/// Remove the file (or, with `directory`, the empty directory) `(dirfd, path)`
/// names.
fn unlink_at(dirfd: u64, path: u64, directory: bool) -> u64 {
    let raw = match read_path(path) {
        Ok(raw) => raw,
        Err(code) => return code,
    };
    // `rmdir(".")` is refused, not resolved: see `names_dot`.
    if directory && names_dot(&raw) {
        return err(EINVAL);
    }
    let path = match resolve_at(dirfd, &raw) {
        Ok(path) => path,
        Err(code) => return code,
    };
    let result = if directory {
        crate::fs::abi_rmdir(Id::current(), &path)
    } else {
        crate::fs::abi_unlink(Id::current(), &path)
    };
    match result {
        Ok(()) => 0,
        Err(error) => fs_err(error),
    }
}

/// `rename(oldpath, newpath)`.
pub(super) fn sys_rename(from: u64, to: u64) -> u64 {
    sys_renameat(AT_FDCWD, from, AT_FDCWD, to)
}

/// `renameat(olddirfd, oldpath, newdirfd, newpath)`.
pub(super) fn sys_renameat(from_dirfd: u64, from: u64, to_dirfd: u64, to: u64) -> u64 {
    match (user_path(from_dirfd, from), user_path(to_dirfd, to)) {
        (Ok(from), Ok(to)) => rename_paths(&from, &to),
        (Err(code), _) | (_, Err(code)) => code,
    }
}

fn rename_paths(from: &str, to: &str) -> u64 {
    match crate::fs::abi_rename(Id::current(), from, to) {
        Ok(()) => 0,
        Err(error) => fs_err(error),
    }
}

/// Whether the caller may use `path` as `mask` (`vfs::READ`/`WRITE`/`EXECUTE`)
/// asks: the ABI mounts first, then the fabricated entries, which carry a mode
/// of their own. `NotFound` when nothing is there.
pub(super) fn check_path(path: &str, mask: u8) -> Result<(), FsError> {
    let id = Id::current();
    match crate::fs::abi_check(id, path, mask) {
        Ok(_) => Ok(()),
        Err(FsError::NotFound) => match synthetic_meta(path) {
            Some(meta) => vfs::check_access(&meta, id, mask),
            None => Err(FsError::NotFound),
        },
        Err(error) => Err(error),
    }
}

/// `access(path, mode)`: POSIX mode bits (`F_OK`=0, `X_OK`=1, `W_OK`=2,
/// `R_OK`=4) line up with the VFS masks, so they pass straight through.
pub(super) fn sys_access(path: u64, mode: u64) -> u64 {
    match user_path(AT_FDCWD, path) {
        Ok(path) => match check_path(&path, (mode & 0o7) as u8) {
            Ok(()) => 0,
            Err(error) => fs_err(error),
        },
        Err(code) => code,
    }
}

/// `umask(mask)`: set the ABI creation mask, return the previous one.
pub(super) fn sys_umask(mask: u64) -> u64 {
    crate::fs::abi_set_umask((mask & 0o777) as u16) as u64
}

/// `readlink(path, buf, size)`: the links we have are `/proc/self/exe`, which
/// resolves to the BusyBox binary (so the shell can re-exec its applets), and
/// `/proc/self/cwd`, the working directory.
pub(super) fn sys_readlink(path: u64, buf: u64, size: u64) -> u64 {
    let target = match user_path(AT_FDCWD, path).as_deref() {
        Ok("/proc/self/exe") => String::from("/busybox"),
        Ok("/proc/self/cwd") => task::cwd(),
        Ok(_) => return err(ENOENT),
        Err(code) => return *code,
    };
    if size == 0 {
        return err(EINVAL);
    }
    let n = (size as usize).min(target.len());
    // Safety: user buffer of at least `n` bytes (the syscall ABI's contract).
    unsafe { user_ptr::copy_to(buf, &target.as_bytes()[..n]) };
    n as u64
}
