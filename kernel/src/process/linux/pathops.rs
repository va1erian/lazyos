//! The naming syscalls that don't open a descriptor: `mkdir`/`mkdirat`,
//! `rmdir`, `unlink`/`unlinkat`, `rename`/`renameat`, `access`, `umask`,
//! `readlink`, and `getcwd`. Split out of [`super::path`] (which keeps path
//! resolution and `open`/`openat`) purely to stay under the file size limit;
//! the two are one responsibility and share its helpers freely.

use crate::fs::vfs::{self, FsError, Id};
use crate::user_ptr;

use super::errno::{err, fs_err, EINVAL, ENOENT};
use super::path::{resolve_at, synthetic_meta};
use super::uaccess::read_cstr;

/// `unlinkat(2)` flag: remove a directory instead of a file.
const AT_REMOVEDIR: u64 = 0x200;

/// `mkdir(path, mode)`.
pub(super) fn sys_mkdir(path: u64, mode: u64) -> u64 {
    match read_cstr(path) {
        Some(path) => mkdir_path(&path, mode),
        None => err(EINVAL),
    }
}

/// `mkdirat(dirfd, path, mode)`.
pub(super) fn sys_mkdirat(dirfd: u64, path: u64, mode: u64) -> u64 {
    match read_cstr(path) {
        Some(path) => match resolve_at(dirfd, &path) {
            Ok(path) => mkdir_path(&path, mode),
            Err(error) => err(error),
        },
        None => err(EINVAL),
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
    match read_cstr(path) {
        Some(path) => match crate::fs::abi_rmdir(Id::current(), &path) {
            Ok(()) => 0,
            Err(error) => fs_err(error),
        },
        None => err(EINVAL),
    }
}

/// `unlink(path)`.
pub(super) fn sys_unlink(path: u64) -> u64 {
    match read_cstr(path) {
        Some(path) => match crate::fs::abi_unlink(Id::current(), &path) {
            Ok(()) => 0,
            Err(error) => fs_err(error),
        },
        None => err(EINVAL),
    }
}

/// `unlinkat(dirfd, path, flags)`: `AT_REMOVEDIR` selects `rmdir` semantics.
pub(super) fn sys_unlinkat(dirfd: u64, path: u64, flags: u64) -> u64 {
    match read_cstr(path) {
        Some(path) => match resolve_at(dirfd, &path) {
            Ok(path) => {
                let result = if flags & AT_REMOVEDIR != 0 {
                    crate::fs::abi_rmdir(Id::current(), &path)
                } else {
                    crate::fs::abi_unlink(Id::current(), &path)
                };
                match result {
                    Ok(()) => 0,
                    Err(error) => fs_err(error),
                }
            }
            Err(error) => err(error),
        },
        None => err(EINVAL),
    }
}

/// `rename(oldpath, newpath)`.
pub(super) fn sys_rename(from: u64, to: u64) -> u64 {
    match (read_cstr(from), read_cstr(to)) {
        (Some(from), Some(to)) => rename_paths(&from, &to),
        _ => err(EINVAL),
    }
}

/// `renameat(olddirfd, oldpath, newdirfd, newpath)`.
pub(super) fn sys_renameat(from_dirfd: u64, from: u64, to_dirfd: u64, to: u64) -> u64 {
    let (Some(from), Some(to)) = (read_cstr(from), read_cstr(to)) else {
        return err(EINVAL);
    };
    match (resolve_at(from_dirfd, &from), resolve_at(to_dirfd, &to)) {
        (Ok(from), Ok(to)) => rename_paths(&from, &to),
        (Err(error), _) | (_, Err(error)) => err(error),
    }
}

fn rename_paths(from: &str, to: &str) -> u64 {
    match crate::fs::abi_rename(Id::current(), from, to) {
        Ok(()) => 0,
        Err(error) => fs_err(error),
    }
}

/// `access(path, mode)`: POSIX mode bits (`F_OK`=0, `X_OK`=1, `W_OK`=2,
/// `R_OK`=4) line up with the VFS masks, so they pass straight through.
pub(super) fn sys_access(path: u64, mode: u64) -> u64 {
    let Some(path) = read_cstr(path) else {
        return err(EINVAL);
    };
    let id = Id::current();
    let mask = (mode & 0o7) as u8;
    match crate::fs::abi_check(id, &path, mask) {
        Ok(_) => 0,
        Err(FsError::NotFound) => match synthetic_meta(&path) {
            Some(meta) => match vfs::check_access(&meta, id, mask) {
                Ok(()) => 0,
                Err(error) => fs_err(error),
            },
            None => err(ENOENT),
        },
        Err(error) => fs_err(error),
    }
}

/// `umask(mask)`: set the ABI creation mask, return the previous one.
pub(super) fn sys_umask(mask: u64) -> u64 {
    crate::fs::abi_set_umask((mask & 0o777) as u16) as u64
}

/// `readlink(path, buf, size)`: the only link we have is `/proc/self/exe`,
/// which resolves to the BusyBox binary (so the shell can re-exec its applets).
pub(super) fn sys_readlink(path: u64, buf: u64, size: u64) -> u64 {
    let target: &[u8] = match read_cstr(path).as_deref() {
        Some("/proc/self/exe") => b"/busybox",
        Some("/proc/self/cwd") => b"/",
        _ => return err(ENOENT),
    };
    if size == 0 {
        return err(EINVAL);
    }
    let n = (size as usize).min(target.len());
    // Safety: user buffer of at least `n` bytes (the syscall ABI's contract).
    unsafe { user_ptr::copy_to(buf, &target[..n]) };
    n as u64
}

pub(super) fn sys_getcwd(buf: u64, size: u64) -> u64 {
    if size < 2 {
        return err(EINVAL);
    }
    // Safety: user buffer (the syscall ABI's contract).
    unsafe {
        user_ptr::write::<u8>(buf, b'/');
        user_ptr::write::<u8>(buf + 1, 0);
    }
    buf
}
