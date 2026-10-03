//! Native filesystem syscalls 15-22 (and 21, `power`) (issue #6): the small, path-based surface
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
//! | 21 | `power` | op | arg | - | (see [`super::power`]) |
//! | 22 | `fsync` | path | - | - | 0 |
//! | 28 | `append_file` | path | data | data length | bytes written |
//! | 30 | `read_at` | path | request `[buf, len, offset]` (3 x u64) | - | bytes read |
//! | 32 | `chmod` | path | mode (`0o7777` bits only) | - | 0 |
//!
//! Every call goes through the native VFS as the calling task, so the
//! permission checks, the read-only FAT boot volume (`-EROFS`) and the writable
//! `/tmp` ramfs behave exactly as they do for the kernel loader. Errors are
//! `-errno` in the return register; every user pointer is validated by
//! `user_ptr` and every path length is bounded, so a hostile caller gets an
//! error, never a kernel fault.

use alloc::format;
use alloc::string::String;

use crate::fs::vfs::{AttrRequest, FileKind, FsError, Id};
use crate::{fs, user_ptr};

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const ENOMEM: i64 = 12;
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
        FsError::NotPermitted => EPERM,
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

/// Dispatch native syscall `nr` (15-22, 28 `append_file`, 30 `read_at` and
/// 32 `chmod`).
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
        21 => return super::power::dispatch(a1, a2),
        22 => path_arg(a1).and_then(|path| fsync(&path)),
        28 => append_file(a1, a2, a3),
        30 => read_at(a1, a2),
        32 => chmod(a1, a2),
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
    let data = user_ptr::try_read_vec(data_ptr, len as usize).map_err(|_| failed(EFAULT))?;
    let id = Id::current();
    match fs::vfs_create(id, &path, FILE_MODE) {
        Ok(_) | Err(FsError::Exists) => {}
        Err(error) => return Err(failed(errno_of(error))),
    }
    fs::vfs_truncate(id, &path, 0).map_err(|e| failed(errno_of(e)))?;
    let written = fs::vfs_write(id, &path, 0, &data).map_err(|e| failed(errno_of(e)))?;
    Ok(written as u64)
}

/// Append the caller's bytes to the end of `path`, creating it when absent.
///
/// `write_file` replaces a whole file and is capped at [`MAX_WRITE`], so a
/// larger file (the package installer's 3 MiB binary) is written as one
/// `write_file` of the first chunk and an `append_file` per further chunk. Like
/// `write_file` the data is read before anything is touched, so a bad pointer
/// never creates or grows a file. Chunks already appended stay if a later one
/// fails: the caller owns cleanup.
fn append_file(path_ptr: u64, data_ptr: u64, len: u64) -> Result<u64, u64> {
    let path = path_arg(path_ptr)?;
    if len > MAX_WRITE {
        return Err(failed(ENOSPC));
    }
    let data = user_ptr::try_read_vec(data_ptr, len as usize).map_err(|_| failed(EFAULT))?;
    let id = Id::current();
    let end = match fs::vfs_stat(id, &path) {
        Ok(meta) if meta.kind == FileKind::Dir => return Err(failed(EISDIR)),
        Ok(meta) => meta.size,
        Err(FsError::NotFound) => {
            fs::vfs_create(id, &path, FILE_MODE).map_err(|e| failed(errno_of(e)))?;
            0
        }
        Err(error) => return Err(failed(errno_of(error))),
    };
    let written = fs::vfs_write(id, &path, end, &data).map_err(|e| failed(errno_of(e)))?;
    Ok(written as u64)
}

/// Read up to `len` bytes of `path` at `offset` into the caller's `buf`, as
/// described by the three-word request at `request_ptr`; returns the count
/// (0 at or past the end of the file).
///
/// Syscall 3 (`read_file`) loads a whole file into the kernel heap before
/// copying it out, which bounds what it can serve by that heap. This call reads
/// only the asked-for range through the VFS, capped at [`MAX_WRITE`] per call
/// so one call never pins more than that, so a caller can stream a file of any
/// size (the package installer reads `.lzp` archives this way). The request is
/// read before anything else, and the bytes are copied out only after the read
/// succeeded, so a bad pointer is `EFAULT` and never a partial result.
fn read_at(path_ptr: u64, request_ptr: u64) -> Result<u64, u64> {
    let path = path_arg(path_ptr)?;
    let word = |index| user_ptr::try_read_at::<u64>(request_ptr, index).map_err(|_| failed(EFAULT));
    let (buf, len, offset) = (word(0)?, word(1)?, word(2)?);
    let mut data = fs::fallible::zeroed(len.min(MAX_WRITE)).map_err(|_| failed(ENOMEM))?;
    let read = fs::vfs_read_at(Id::current(), &path, offset, &mut data)
        .map_err(|e| failed(errno_of(e)))?;
    user_ptr::try_copy_to(buf, &data[..read]).map_err(|_| failed(EFAULT))?;
    Ok(read as u64)
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

/// Flush the filesystem holding `path` to stable storage. The path must
/// resolve; the flush is per-mount, so `confd` fsyncs its temporary file before
/// renaming it over the committed store.
fn fsync(path: &str) -> Result<u64, u64> {
    fs::vfs_flush(Id::current(), path)
        .map(|_| 0)
        .map_err(|e| failed(errno_of(e)))
}

/// The permission bits `chmod` may set: rwx for owner, group and other, plus
/// setuid, setgid and sticky. The type bits are never the caller's to change.
const MODE_BITS: u64 = 0o7777;

/// Set the permission bits of `path` (syscall 31). Exactly the Linux `chmod`
/// rules, because both go through `Vfs::setattr`: only the owner or root
/// (`-EPERM` otherwise), a read-only mount is `-EROFS`, setgid is dropped for
/// a caller outside the file's group. Unlike Linux, a mode with any bit above
/// `0o7777` is `-EINVAL` rather than silently masked: the native ABI is new and
/// a stray type bit is a caller bug worth reporting. The mode is checked
/// before the path is read, so a bad mode never touches the file.
fn chmod(path_ptr: u64, mode: u64) -> Result<u64, u64> {
    if mode & !MODE_BITS != 0 {
        return Err(failed(EINVAL));
    }
    let path = path_arg(path_ptr)?;
    fs::vfs_setattr(Id::current(), &path, AttrRequest::Mode(mode as u16))
        .map(|_| 0)
        .map_err(|e| failed(errno_of(e)))
}
