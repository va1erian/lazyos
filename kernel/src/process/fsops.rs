//! Native filesystem syscalls 15-20 (and 21, `power`) (issue #6): the small, path-based surface
//! the ring-3 shell needs for `dir`, `copy`, `del`, `ren` and `mkdir`.
//!
//! | nr | call | `rdi` | `rsi` | `rdx` | result |
//! |----|------|-------|-------|-------|--------|
//! | 15 | `stat` | path | out `[size, kind]` (2 x u64) | - | 0 |
//! | 16 | `readdir` | path | buffer | buffer length | bytes written |
//! | 17 | `write_file` | path | data | data length | bytes written |
//! | 18 | `mkdir` | path | - | - | 0 |
//! | 19 | `unlink` | path | - | - | 0 |
//! | 20 | `rename` | from | to | - | 0 |
//! | 21 | `power` | op | - | - | (see [`super::power`]) |
//!
//! Every call goes through the native VFS as the calling task, so the
//! permission checks, the read-only FAT boot volume (`-EROFS`) and the writable
//! `/tmp` ramfs behave exactly as they do for the kernel loader. Errors are
//! `-errno` in the return register; every user pointer is validated by
//! `user_ptr` and every path length is bounded, so a hostile caller gets an
//! error, never a kernel fault.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::{FileKind, FsError, Id};
use crate::{fs, user_ptr};

const ENOENT: i64 = 2;
const EFAULT: i64 = 14;
const EACCES: i64 = 13;
const EEXIST: i64 = 17;
const ENOTDIR: i64 = 20;
const EISDIR: i64 = 21;
const EINVAL: i64 = 22;
const ENOSPC: i64 = 28;
const EROFS: i64 = 30;
const ENAMETOOLONG: i64 = 36;
const ENOSYS: i64 = 38;
const ENOTEMPTY: i64 = 39;

/// Longest path a native fs syscall reads from user memory.
const PATH_MAX: usize = 1024;
/// Largest file `write_file` accepts in one call; bigger copies must be split
/// by the caller, which keeps one syscall from pinning an unbounded buffer.
pub const MAX_WRITE: u64 = 1 << 20;
/// Permissions for files and directories the shell creates.
const FILE_MODE: u16 = 0o644;
const DIR_MODE: u16 = 0o755;

fn failed(errno: i64) -> u64 {
    errno.wrapping_neg() as u64
}

fn errno_of(error: FsError) -> i64 {
    match error {
        FsError::NotFound => ENOENT,
        FsError::Exists => EEXIST,
        FsError::NotDir => ENOTDIR,
        FsError::IsDir => EISDIR,
        FsError::NotEmpty => ENOTEMPTY,
        FsError::Access => EACCES,
        FsError::ReadOnly => EROFS,
        FsError::Invalid => EINVAL,
        FsError::NoSpace => ENOSPC,
        FsError::NameTooLong => ENAMETOOLONG,
        FsError::NotSupported => ENOSYS,
    }
}

/// A path argument, or the `-errno` to return.
fn path_arg(ptr: u64) -> Result<String, u64> {
    // A path with no terminator within `PATH_MAX` is too long, not unreadable:
    // Linux reports `ENAMETOOLONG` here, while only a genuine memory fault is
    // `EFAULT`.
    let bytes = user_ptr::try_cstr(ptr, PATH_MAX).map_err(|err| match err {
        user_ptr::CStrError::Fault => failed(EFAULT),
        user_ptr::CStrError::Unterminated => failed(ENAMETOOLONG),
    })?;
    let path = String::from_utf8(bytes).map_err(|_| failed(EINVAL))?;
    if path.is_empty() {
        return Err(failed(EINVAL));
    }
    Ok(path)
}

/// Dispatch native syscall `nr` (15-20).
pub fn dispatch(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let outcome = match nr {
        15 => stat(a1, a2),
        16 => readdir(a1, a2, a3),
        17 => write_file(a1, a2, a3),
        18 => path_arg(a1).and_then(|path| mkdir(&path)),
        19 => path_arg(a1).and_then(|path| unlink(&path)),
        20 => path_arg(a1)
            .and_then(|from| Ok((from, path_arg(a2)?)))
            .and_then(|(from, to)| rename(&from, &to)),
        21 => return super::power::dispatch(a1),
        _ => return u64::MAX,
    };
    outcome.unwrap_or_else(|code| code)
}

fn stat(path_ptr: u64, out: u64) -> Result<u64, u64> {
    let path = path_arg(path_ptr)?;
    let meta = fs::vfs_stat(Id::current(), &path).map_err(|e| failed(errno_of(e)))?;
    let kind = u64::from(meta.kind == FileKind::Dir);
    user_ptr::try_copy_words(out, &[meta.size, kind]).map_err(|_| failed(EFAULT))?;
    Ok(0)
}

/// One `"<d|f> <size> <name>\n"` line per entry. Only whole lines are written,
/// so a short buffer truncates the listing cleanly. Names holding a newline
/// cannot be framed and are skipped.
fn readdir(path_ptr: u64, buf: u64, len: u64) -> Result<u64, u64> {
    let path = path_arg(path_ptr)?;
    let id = Id::current();
    let entries = fs::vfs_readdir(id, &path).map_err(|e| failed(errno_of(e)))?;
    let base = path.trim_end_matches('/');
    let mut listing = String::new();
    for entry in entries {
        if entry.name.contains('\n') {
            continue;
        }
        let child = format!("{base}/{}", entry.name);
        let size = fs::vfs_stat(id, &child).map(|meta| meta.size).unwrap_or(0);
        let tag = if entry.kind == FileKind::Dir {
            'd'
        } else {
            'f'
        };
        let line = format!("{tag} {size} {}\n", entry.name);
        if listing.len() + line.len() > len as usize {
            break;
        }
        listing.push_str(&line);
    }
    user_ptr::try_copy_to(buf, listing.as_bytes()).map_err(|_| failed(EFAULT))?;
    Ok(listing.len() as u64)
}

/// Create-or-replace `path` with the caller's bytes. The data is read before
/// anything is touched, so a bad pointer never truncates an existing file.
fn write_file(path_ptr: u64, data_ptr: u64, len: u64) -> Result<u64, u64> {
    let path = path_arg(path_ptr)?;
    if len > MAX_WRITE {
        return Err(failed(ENOSPC));
    }
    let data: Vec<u8> = user_ptr::try_bytes(data_ptr, len as usize)
        .map_err(|_| failed(EFAULT))?
        .to_vec();
    let id = Id::current();
    match fs::vfs_create(id, &path, FILE_MODE) {
        Ok(_) | Err(FsError::Exists) => {}
        Err(error) => return Err(failed(errno_of(error))),
    }
    fs::vfs_truncate(id, &path, 0).map_err(|e| failed(errno_of(e)))?;
    let written = fs::vfs_write(id, &path, 0, &data).map_err(|e| failed(errno_of(e)))?;
    Ok(written as u64)
}

fn mkdir(path: &str) -> Result<u64, u64> {
    fs::vfs_mkdir(Id::current(), path, DIR_MODE)
        .map(|_| 0)
        .map_err(|e| failed(errno_of(e)))
}

/// Remove a file, or an empty directory.
fn unlink(path: &str) -> Result<u64, u64> {
    let id = Id::current();
    let meta = fs::vfs_stat(id, path).map_err(|e| failed(errno_of(e)))?;
    let result = if meta.kind == FileKind::Dir {
        fs::vfs_rmdir(id, path)
    } else {
        fs::vfs_unlink(id, path)
    };
    result.map(|_| 0).map_err(|e| failed(errno_of(e)))
}

fn rename(from: &str, to: &str) -> Result<u64, u64> {
    fs::vfs_rename(Id::current(), from, to)
        .map(|_| 0)
        .map_err(|e| failed(errno_of(e)))
}
