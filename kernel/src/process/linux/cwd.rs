//! The working directory of the Linux ABI and the one path resolver.
//!
//! Every syscall that names a file goes through [`resolve_at`] (directly, or
//! through [`user_path`], which also reads the user string), so what a
//! relative path means is decided in exactly one place: it is joined onto the
//! caller's directory ([`task::cwd`], or the directory a descriptor was
//! opened on), folded lexically (`.` and `..` vanish, `..` clamps at the
//! root) and handed to the VFS as an absolute path. Nothing below this layer
//! ever sees a relative one. There are no symlinks, so the lexical fold is
//! also the real answer.
//!
//! The directory itself is a string in the task ([`task::set_cwd`]), not a
//! handle: it is cheap to inherit on `fork`, survives `execve` for free, and
//! pins nothing, so `rmdir` of somebody's current directory is allowed, as on
//! Linux. What happens next is decided lazily. Relative lookups from a
//! removed directory fail with `ENOENT` because the path no longer exists,
//! and `getcwd` reports `ENOENT` for it explicitly. (Unlike Linux, which
//! tracks the inode, a directory re-created under the same name is the cwd
//! again.)

use alloc::format;
use alloc::string::String;

use crate::fs::vfs::{self, FileKind, FsError, Path};
use crate::task::{self, FdKind};
use crate::user_ptr::{self, CStrError};

use super::errno::{err, fs_err, EBADF, EFAULT, ENAMETOOLONG, ENOENT, ENOTDIR, ERANGE};
use super::fd::fd_meta_get;
use super::path::{resolve, Target};
use super::pathops::check_path;

/// `openat(AT_FDCWD, ...)` sentinel: resolve against the working directory.
pub(super) const AT_FDCWD: u64 = (-100i64) as u64;

/// The longest path (NUL included) a resolved name or the cwd may have.
pub(super) const PATH_MAX: usize = 4096;

/// Resolve a `(dirfd, path)` pair into an absolute, normalized ABI path.
///
/// An absolute `path` ignores `dirfd`. A relative one joins onto the working
/// directory for `AT_FDCWD`, and otherwise onto the directory `dirfd` was
/// opened on, so `std`'s fd-relative `openat`/`unlinkat` walks work. The empty
/// path names that directory itself. A result that would not fit in
/// `PATH_MAX` is `ENAMETOOLONG`: without the bound, repeated relative
/// `chdir`s could grow the cwd without limit.
pub(super) fn resolve_at(dirfd: u64, path: &str) -> Result<String, u64> {
    let base = if path.starts_with('/') {
        String::new()
    } else if dirfd == AT_FDCWD {
        task::cwd()
    } else {
        dir_path_of_fd(dirfd)?
    };
    let folded = Path::parse(&format!("{base}/{path}")).to_path_string();
    if folded.len() >= PATH_MAX {
        return Err(err(ENAMETOOLONG));
    }
    Ok(folded)
}

/// Read the user path at `ptr`: unreadable memory is `-EFAULT`, a path with no
/// NUL in `PATH_MAX` bytes `-ENAMETOOLONG`, and one that is not UTF-8 names
/// nothing (`-ENOENT`), as no file here can have such a name.
pub(super) fn read_path(ptr: u64) -> Result<String, u64> {
    match user_ptr::try_cstr(ptr, PATH_MAX) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| err(ENOENT)),
        Err(CStrError::Fault) => Err(err(EFAULT)),
        Err(CStrError::Unterminated) => Err(err(ENAMETOOLONG)),
    }
}

/// Read a user path and resolve it against `dirfd`.
pub(super) fn user_path(dirfd: u64, ptr: u64) -> Result<String, u64> {
    resolve_at(dirfd, &read_path(ptr)?)
}

/// The directory an open descriptor stands for. `EBADF` when there is no such
/// descriptor (or it carries no path), `ENOTDIR` when it is open on something
/// that is not a directory.
pub(super) fn dir_path_of_fd(fd: u64) -> Result<String, u64> {
    let fd = usize::try_from(fd).map_err(|_| err(EBADF))?;
    match task::fd_kind(fd) {
        FdKind::Closed => Err(err(EBADF)),
        // A directory is a snapshot descriptor whose side-table entry records
        // the path it was opened on; that entry is missing for a descriptor
        // inherited across `fork`, which cannot be used as a directory yet.
        FdKind::File => {
            let meta = fd_meta_get(fd).ok_or(err(EBADF))?;
            let is_dir = meta.mode & u32::from(vfs::S_IFMT) == u32::from(vfs::S_IFDIR);
            match meta.path {
                Some(path) if is_dir => Ok(path),
                _ => Err(err(ENOTDIR)),
            }
        }
        _ => Err(err(ENOTDIR)),
    }
}

/// Whether the last component of `raw` is `.`. The resolver folds that away,
/// so `rmdir(".")` would otherwise delete the directory under its own name
/// instead of failing with `EINVAL` as POSIX says.
pub(super) fn names_dot(raw: &str) -> bool {
    raw.trim_end_matches('/').rsplit('/').next() == Some(".")
}

/// `chdir(path)`.
pub(super) fn sys_chdir(path: u64) -> u64 {
    match user_path(AT_FDCWD, path) {
        Ok(path) => enter(&path),
        Err(code) => code,
    }
}

/// `fchdir(fd)`: `fd` must be open on a directory.
pub(super) fn sys_fchdir(fd: u64) -> u64 {
    match dir_path_of_fd(fd) {
        Ok(path) => enter(&path),
        Err(code) => code,
    }
}

/// Make `path` the working directory if it is a directory the caller may
/// search. The type is checked before the permission, as Linux does.
fn enter(path: &str) -> u64 {
    let meta = match resolve(path) {
        Ok(Target::Node(meta)) | Ok(Target::Synthetic(meta)) => meta,
        Err(error) => return fs_err(error),
    };
    if meta.kind != FileKind::Dir {
        return err(ENOTDIR);
    }
    if let Err(error) = check_path(path, vfs::EXECUTE) {
        return fs_err(error);
    }
    task::set_cwd(path);
    0
}

/// Whether the directory `cwd` no longer exists as a directory. Only a
/// vanished name counts: a lost search bit on an ancestor does not stop a
/// process from knowing where it is.
fn is_removed(cwd: &str) -> bool {
    match resolve(cwd) {
        Ok(Target::Node(meta)) | Ok(Target::Synthetic(meta)) => meta.kind != FileKind::Dir,
        Err(error) => matches!(error, FsError::NotFound | FsError::NotDir),
    }
}

/// `getcwd(buf, size)` as the raw syscall behaves: the path and a NUL are
/// stored at `buf`, and the result is the number of bytes stored. `ERANGE`
/// when `size` cannot hold them, `ENOENT` when the directory has been removed,
/// `EFAULT` for a buffer that is not writable user memory (nothing is written
/// then).
pub(super) fn sys_getcwd(buf: u64, size: u64) -> u64 {
    let cwd = task::cwd();
    if is_removed(&cwd) {
        return err(ENOENT);
    }
    let mut bytes = cwd.into_bytes();
    bytes.push(0);
    if bytes.len() as u64 > size {
        return err(ERANGE);
    }
    match user_ptr::try_copy_to(buf, &bytes) {
        Ok(()) => bytes.len() as u64,
        Err(_) => err(EFAULT),
    }
}
