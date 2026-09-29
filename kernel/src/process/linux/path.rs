//! Path resolution and `open`/`openat`. The other syscalls that merely name a
//! path (`mkdir`, `rename`, `access`, `readlink`, ...) are in
//! [`super::pathops`]; this module is the one every one of them still goes
//! through, since [`resolve`] is the one place that decides what a Linux path
//! means: the ABI VFS mounts first (a copy-up overlay at `/`, a shared ramfs
//! at `/tmp`), then the kernel-fabricated entries this module invents —
//! synthetic directories ([`synthetic_dir`]) and BusyBox applet aliases
//! ([`applet_name`]) — so that `execvp("ls")` and `$PATH` lookups work
//! without a real `/bin` on disk.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::{self, FileKind, FsError, Id, Meta};
use crate::task::{self, Fd, FdKind};

use super::errno::{err, fs_err, EBADF, EEXIST, EINVAL, EISDIR, ENOENT, ENOTDIR, EROFS};
use super::fd::{fd_meta_get, file_meta, open_device_fd, open_snapshot};
use super::uaccess::read_cstr;

/// `openat(AT_FDCWD, ...)` sentinel.
pub(super) const AT_FDCWD: u64 = (-100i64) as u64;

/// `openat(2)` access mode mask.
const O_ACCMODE: u64 = 0o3;
/// `openat(2)` flag bits (Linux x86_64 values).
const O_CREAT: u64 = 0o100;
const O_EXCL: u64 = 0o200;
const O_TRUNC: u64 = 0o1000;
const O_APPEND: u64 = 0o2000;
const O_DIRECTORY: u64 = 0o200000;

/// A bare applet name in a `bin` directory (or with no directory) that isn't a
/// real FAT file aliases to the BusyBox binary.
fn applet_name(path: &str) -> Option<&str> {
    let trimmed = path.trim_start_matches('/');
    let base = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let dir = &trimmed[..trimmed.len() - base.len()];
    let plain = !base.is_empty()
        && base.len() <= 12
        && !base.contains('.')
        && base
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if plain && (dir.is_empty() || dir.contains("bin")) {
        Some(base)
    } else {
        None
    }
}

/// The synthetic root directories that have no filesystem behind them yet
/// (`/tmp` is a real ramfs mount and resolves through the VFS).
fn synthetic_dir(path: &str) -> bool {
    matches!(
        path,
        "/" | "/bin" | "/sbin" | "/usr" | "/dev" | "/proc" | "/etc"
    )
}

/// Metadata for a kernel-fabricated entry: a synthetic directory or a BusyBox
/// applet alias. Only used when the VFS has no node at `path`.
pub(super) fn synthetic_meta(path: &str) -> Option<Meta> {
    if synthetic_dir(path) {
        return Some(Meta {
            ino: 1,
            mode: vfs::S_IFDIR | 0o755,
            uid: 0,
            gid: 0,
            size: 0,
            kind: FileKind::Dir,
        });
    }
    if applet_name(path).is_some() {
        return crate::fs::abi_stat(Id::current(), "/busybox")
            .ok()
            .map(|meta| Meta {
                ino: 0,
                mode: vfs::S_IFREG | 0o555,
                uid: 0,
                gid: 0,
                size: meta.size,
                kind: FileKind::File,
            });
    }
    None
}

/// Where a Linux path leads: a node on a mount, or a kernel-fabricated entry.
pub(super) enum Target {
    Node(Meta),
    Synthetic(Meta),
}

/// Resolve a path the way the Linux ABI sees it: the ABI VFS mounts first (a
/// copy-up overlay at `/`, a shared ramfs at `/tmp`), then the synthetic
/// directories and applet aliases. A permission error from the VFS is returned
/// as-is; only a miss falls through to the fabricated entries.
pub(super) fn resolve(path: &str) -> Result<Target, FsError> {
    match crate::fs::abi_stat(Id::current(), path) {
        Ok(meta) => return Ok(Target::Node(meta)),
        Err(FsError::NotFound) => {}
        Err(error) => return Err(error),
    }
    synthetic_meta(path)
        .map(Target::Synthetic)
        .ok_or(FsError::NotFound)
}

/// Load a file's bytes through the ABI VFS, with the BusyBox applet alias.
pub(super) fn load_file(path: &str) -> Result<Vec<u8>, FsError> {
    load_file_as(Id::current(), path)
}

/// [`load_file`] with an explicit caller identity (used by `open_path`).
fn load_file_as(id: Id, path: &str) -> Result<Vec<u8>, FsError> {
    match crate::fs::abi_read(id, path) {
        Ok(data) => Ok(data),
        Err(FsError::NotFound) if applet_name(path).is_some() => {
            crate::fs::abi_read(id, "/busybox").map_err(|_| FsError::NotFound)
        }
        Err(error) => Err(error),
    }
}

/// The image-root program an applet-shaped name stands for: `rhai`,
/// `/usr/local/bin/rhai` and `/bin/rhai` all mean `/RHAI.ELF` (issue #319).
/// The FAT root only holds 8.3 names, so longer names never match, and the
/// mandatory `.ELF` keeps data files (`PASSWD`, `HELLO.TXT`) from shadowing a
/// BusyBox applet of the same name.
fn root_elf_path(path: &str) -> Option<String> {
    let base = applet_name(path)?;
    (base.len() <= 8).then(|| format!("/{}.ELF", base.to_ascii_uppercase()))
}

/// Load an executable for `execve`. In order:
///
/// 1. the file at `path` itself;
/// 2. for an applet-shaped name (`rhai`, `/bin/rhai`), the program of that
///    name at the image root ([`root_elf_path`]) — this must precede the
///    BusyBox alias, which would otherwise claim every plain name in a `bin`
///    directory;
/// 3. the BusyBox applet alias, and — when a `$PATH` lookup names one of the
///    synthetic `bin` directories LazyOS does not back with files — the
///    basename at the image root. The executable store is the flat FAT root,
///    so this is what lets `execvp("INIT.ELF")` find `/INIT.ELF` after trying
///    `/usr/local/bin`, `/bin` and `/usr/bin`.
pub(super) fn load_executable(path: &str) -> Result<Vec<u8>, FsError> {
    let id = Id::current();
    match crate::fs::abi_read(id, path) {
        Ok(elf) => return Ok(elf),
        Err(FsError::NotFound) => {}
        Err(error) => return Err(error),
    }
    if let Some(root) = root_elf_path(path) {
        match crate::fs::abi_read(id, &root) {
            Ok(elf) => return Ok(elf),
            Err(FsError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    match load_file(path) {
        Ok(elf) => Ok(elf),
        Err(FsError::NotFound) => {
            let base = path.rsplit('/').next().unwrap_or(path);
            if base != path && !base.is_empty() {
                load_file(base)
            } else {
                Err(FsError::NotFound)
            }
        }
        Err(error) => Err(error),
    }
}

fn push_dirent(out: &mut Vec<u8>, ino: u64, d_type: u8, name: &str) {
    let start = out.len();
    out.extend_from_slice(&ino.to_le_bytes()); // d_ino
    out.extend_from_slice(&0u64.to_le_bytes()); // d_off
    out.extend_from_slice(&0u16.to_le_bytes()); // d_reclen (patched below)
    out.push(d_type);
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    while !(out.len() - start).is_multiple_of(8) {
        out.push(0);
    }
    let reclen = (out.len() - start) as u16;
    out[start + 16..start + 18].copy_from_slice(&reclen.to_le_bytes());
}

/// The `linux_dirent64` type byte for a VFS node kind.
fn dtype_of(kind: FileKind) -> u8 {
    const DT_DIR: u8 = 4;
    const DT_REG: u8 = 8;
    match kind {
        FileKind::Dir => DT_DIR,
        FileKind::File => DT_REG,
    }
}

/// The `.`/`..` prefix every directory stream starts with.
fn empty_dir_stream() -> Vec<u8> {
    const DT_DIR: u8 = 4;
    let mut out = Vec::new();
    push_dirent(&mut out, 1, DT_DIR, ".");
    push_dirent(&mut out, 1, DT_DIR, "..");
    out
}

/// Build a `linux_dirent64` stream for a directory, so `getdents64` can read
/// it like a file (the fd table stores byte snapshots, not directory handles).
/// The ABI VFS supplies the real entries; `.`/`..` are added here.
fn dir_stream(path: &str) -> Result<Vec<u8>, FsError> {
    let mut out = empty_dir_stream();
    for entry in crate::fs::abi_readdir(Id::current(), path)? {
        push_dirent(&mut out, entry.ino, dtype_of(entry.kind), &entry.name);
    }
    Ok(out)
}

/// Refuse a write-mode open of a fabricated entry (a synthetic directory or a
/// BusyBox applet alias): check the mount's write permission first (so a
/// denial is `EACCES`), then answer `EROFS` because there is no backing node.
fn write_open_denied(path: &str) -> u64 {
    match crate::fs::abi_check(Id::current(), path, vfs::WRITE) {
        Ok(_) | Err(FsError::NotFound) => {}
        Err(error) => return fs_err(error),
    }
    crate::serial_println!("fs: {path}: {}", FsError::ReadOnly.message());
    err(EROFS)
}

/// Open a directory as a snapshot of its `getdents64` stream.
fn open_dir_fd(path: &str, meta: Meta) -> u64 {
    match dir_stream(path) {
        Ok(data) => open_snapshot(data, file_meta(meta, String::from(path), false, false)),
        Err(error) => fs_err(error),
    }
}

/// Snapshot a file and open it with the requested access mode. Writable
/// descriptors record the backing path so `write(2)` reaches the ABI VFS.
fn open_file_fd(id: Id, path: &str, meta: Meta, writable: bool, append: bool) -> u64 {
    match load_file_as(id, path) {
        Ok(data) => open_snapshot(data, file_meta(meta, String::from(path), writable, append)),
        Err(error) => fs_err(error),
    }
}

/// Open a path through the ABI VFS (the copy-up overlay root and the shared
/// `/tmp` ramfs), plus the synthetic device nodes and BusyBox applet aliases.
/// Honours `O_CREAT`, `O_EXCL`, `O_TRUNC`, `O_APPEND`, and `O_DIRECTORY`.
fn open_path(path: &str, flags: u64, mode: u64) -> u64 {
    match path {
        "/dev/tty" | "/dev/console" | "/dev/tty0" | "/dev/tty1" => {
            return super::fd::fd_result(task::fd_open(Fd::Terminal));
        }
        "/dev/null" | "/dev/zero" | "/dev/full" => {
            return open_device_fd();
        }
        _ => {}
    }

    let id = Id::current();
    let write_access = flags & O_ACCMODE != 0;
    let create = flags & O_CREAT != 0;
    let exclusive = flags & O_EXCL != 0;
    let truncate = flags & O_TRUNC != 0;
    let append = flags & O_APPEND != 0;
    let directory = flags & O_DIRECTORY != 0;
    // A missing mode argument (the legacy `open` dispatch and tests) defaults
    // to the usual 0o666; musl passes the caller's mode through `openat`.
    let mode = match (mode & 0o7777) as u16 {
        0 => 0o666,
        mode => mode,
    };

    let existing = match resolve(path) {
        Ok(Target::Node(meta)) => Some(meta),
        Ok(Target::Synthetic(meta)) => {
            // Fabricated entries cannot be created or written through.
            if write_access || truncate || create {
                return write_open_denied(path);
            }
            return if meta.kind == FileKind::Dir {
                open_dir_fd(path, meta)
            } else {
                open_file_fd(id, path, meta, false, false)
            };
        }
        Err(FsError::NotFound) => None,
        Err(error) => return fs_err(error),
    };

    if let Some(meta) = existing {
        if create && exclusive {
            return err(EEXIST);
        }
        if directory && meta.kind != FileKind::Dir {
            return err(ENOTDIR);
        }
        if meta.kind == FileKind::Dir {
            if write_access || truncate {
                return err(EISDIR);
            }
            return open_dir_fd(path, meta);
        }
        if write_access {
            if let Err(error) = crate::fs::abi_check(id, path, vfs::WRITE) {
                return fs_err(error);
            }
            if truncate {
                if let Err(error) = crate::fs::abi_truncate(id, path, 0) {
                    return fs_err(error);
                }
            }
        }
        return open_file_fd(id, path, meta, write_access, append);
    }

    if !create {
        return err(ENOENT);
    }
    let created = if directory {
        crate::fs::abi_mkdir(id, path, mode)
    } else {
        crate::fs::abi_create(id, path, mode)
    };
    if let Err(error) = created {
        return fs_err(error);
    }
    match resolve(path) {
        Ok(Target::Node(meta)) => open_file_fd(id, path, meta, write_access, append),
        _ => err(ENOENT),
    }
}

pub(super) fn sys_openat(dirfd: u64, path: u64, flags: u64, mode: u64) -> u64 {
    match read_cstr(path) {
        Some(path) => match resolve_at(dirfd, &path) {
            Ok(path) => open_path(&path, flags, mode),
            Err(error) => err(error),
        },
        None => err(EINVAL),
    }
}

/// Resolve a `(dirfd, path)` pair into an absolute ABI path. Relative names
/// with a real descriptor join that descriptor's recorded directory path, so
/// `std`'s fd-relative `openat`/`unlinkat` walks work; `AT_FDCWD` roots at `/`.
pub(super) fn resolve_at(dirfd: u64, path: &str) -> Result<String, u64> {
    if path.starts_with('/') {
        return Ok(String::from(path));
    }
    if path.is_empty() {
        return Ok(String::from("/"));
    }
    if dirfd == AT_FDCWD {
        return Ok(format!("/{path}"));
    }
    let fd = dirfd as usize;
    match fd_meta_get(fd).and_then(|meta| meta.path) {
        Some(base) if task::fd_kind(fd) == FdKind::File => {
            if base == "/" {
                Ok(format!("/{path}"))
            } else {
                Ok(format!("{base}/{path}"))
            }
        }
        _ => Err(EBADF),
    }
}

// `mkdir`/`rmdir`/`unlink`/`rename`/`access`/`umask`/`readlink`/`getcwd` live
// in `pathops`, split out purely to stay under the file size limit.
