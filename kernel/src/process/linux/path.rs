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
use crate::process::image::VfsFile;
use crate::task::{self, Fd};

use super::cwd::user_path;
use super::errno::{err, fs_err, EEXIST, EISDIR, ENOENT, ENOTDIR, EROFS};
use super::fd::{file_meta, open_device_fd, open_snapshot};
use super::native::{BIN_DIRS, SYSTEM_BIN_DIR};
use super::vfsfd::open_vfs_fd;

/// `openat(2)` access mode mask.
const O_ACCMODE: u64 = 0o3;
const O_WRONLY: u64 = 0o1;
/// `openat(2)` flag bits (Linux x86_64 values).
const O_CREAT: u64 = 0o100;
const O_EXCL: u64 = 0o200;
const O_TRUNC: u64 = 0o1000;
const O_APPEND: u64 = 0o2000;
const O_DIRECTORY: u64 = 0o200000;

/// How an `open` asked to use the file: the access mode plus `O_APPEND`.
#[derive(Clone, Copy)]
struct Access {
    read: bool,
    write: bool,
    append: bool,
}

impl Access {
    /// Decode `O_ACCMODE` (`O_RDONLY`=0, `O_WRONLY`=1, `O_RDWR`=2) and `O_APPEND`.
    fn from_flags(flags: u64) -> Access {
        Access {
            read: flags & O_ACCMODE != O_WRONLY,
            write: flags & O_ACCMODE != 0,
            append: flags & O_APPEND != 0,
        }
    }

    /// Read-only, as for a fabricated entry.
    const READ_ONLY: Access = Access {
        read: true,
        write: false,
        append: false,
    };
}

/// A plain applet name that isn't a real file aliases to the BusyBox binary,
/// but only where a command is looked up: one of the synthetic `$PATH`
/// directories ([`BIN_DIRS`]: `/bin/ls`, `/usr/local/bin/rhai`), `/system/bin`
/// (a session's `$PATH`, issue #508: `ls` there is BusyBox's while no file of
/// that name exists), or a bare name with no directory at all (the kernel's
/// own `execvp`-style callers).
///
/// Nowhere else: a resolved absolute path at `/` (`/nope`) or in some other
/// directory that merely has `bin` in it (an app's `/apps/<id>/bin/`)
/// must name a real file, or `stat` would report a file that does not exist
/// and `open(O_CREAT)` would refuse to create one there.
fn applet_name(path: &str) -> Option<&str> {
    let (dir, base) = match path.strip_prefix('/') {
        Some(absolute) => {
            let split = absolute.rfind('/').map_or(0, |at| at + 1);
            let (dir, base) = absolute.split_at(split);
            (Some(dir), base)
        }
        None => (None, path),
    };
    let in_bin_dir = match dir {
        Some(dir) => BIN_DIRS.contains(&dir) || dir == SYSTEM_BIN_DIR,
        None => !path.contains('/'),
    };
    let plain = !base.is_empty()
        && base.len() <= 12
        && !base.contains('.')
        && base
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    (plain && in_bin_dir).then_some(base)
}

/// The synthetic root directories that have no filesystem behind them yet
/// (`/tmp` is a real ramfs mount and resolves through the VFS).
fn synthetic_dir(path: &str) -> bool {
    matches!(
        path,
        "/" | "/bin" | "/sbin" | "/usr" | "/dev" | "/dev/pts" | "/proc" | "/proc/self" | "/etc"
    )
}

/// The data devices (`/dev/null` and friends, see [`open_device_fd`]).
pub(super) const DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
];

/// Whether `path` is a fabricated character device node.
fn device_node(path: &str) -> bool {
    DEVICES.contains(&path)
        || matches!(
            path,
            "/dev/tty" | "/dev/console" | "/dev/tty0" | "/dev/tty1" | "/dev/ptmx"
        )
        || path
            .strip_prefix("/dev/pts/")
            .and_then(|n| n.parse::<u32>().ok())
            .is_some_and(|n| crate::tty::pty::Pty::find_slave(n).is_some())
}

/// Metadata for a kernel-fabricated entry: a synthetic directory, a `/proc`
/// file ([`super::procfs`]) or a BusyBox applet alias. Only used when the VFS has no node at `path`.
pub(super) fn synthetic_meta(path: &str) -> Option<Meta> {
    if synthetic_dir(path) {
        return Some(Meta {
            ino: 1,
            mode: vfs::S_IFDIR | 0o755,
            uid: 0,
            gid: 0,
            size: 0,
            kind: FileKind::Dir,
            times: vfs::Times::default(),
        });
    }
    if let Some(meta) = super::procfs::meta(path) {
        return Some(meta);
    }
    if device_node(path) {
        return Some(Meta {
            ino: super::tty::path_ino(path),
            mode: super::flags::S_IFCHR as u16 | 0o666,
            uid: 0,
            gid: 0,
            size: 0,
            kind: FileKind::File,
            times: vfs::Times::default(),
        });
    }
    if applet_name(path).is_some() {
        return crate::fs::abi_stat(Id::current(), fhs::bin::BUSYBOX)
            .ok()
            .map(|meta| Meta {
                ino: 0,
                mode: vfs::S_IFREG | 0o555,
                uid: 0,
                gid: 0,
                size: meta.size,
                kind: FileKind::File,
                times: vfs::Times::default(),
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

/// Load a file's bytes through the ABI VFS as `id`, with the BusyBox applet
/// alias (used by `open_path` for snapshot opens).
fn load_file_as(id: Id, path: &str) -> Result<Vec<u8>, FsError> {
    match crate::fs::abi_read(id, path) {
        Ok(data) => Ok(data),
        Err(FsError::NotFound) if applet_name(path).is_some() => {
            crate::fs::abi_read(id, fhs::bin::BUSYBOX).map_err(|_| FsError::NotFound)
        }
        Err(error) => Err(error),
    }
}

/// The `/system/bin` program an applet-shaped name stands for: `rhai`,
/// `/usr/local/bin/rhai` and `/bin/rhai` all mean `/system/bin/rhai` (issue
/// #319, docs/filesystem-plan.md F3). Only programs live in `/system/bin`, so
/// a data file can never shadow a BusyBox applet of the same name.
///
/// The volume is ext2, which is case-sensitive, and the name is used as typed:
/// `RHAI` is `/system/bin/RHAI`, which does not exist.
pub(super) fn system_bin_path(path: &str) -> Option<String> {
    let base = applet_name(path)?;
    Some(format!("{}/{base}", fhs::SYSTEM_BIN))
}

/// Open an executable for `execve` and spawns, for streaming. In order:
///
/// 1. the file at `path` itself;
/// 2. for an applet-shaped name (`rhai`, `/bin/rhai`), the program of that
///    name in `/system/bin` ([`system_bin_path`]) — this must precede the
///    BusyBox alias, which would otherwise claim every plain name in a `bin`
///    directory;
/// 3. the BusyBox applet alias, and — when a `$PATH` lookup names a
///    directory LazyOS does not back with files — the basename resolved
///    through these same steps (its `/system/bin` program, else its alias,
///    else the file of that name). This is what lets `execvp("rhai")` find
///    `/system/bin/rhai` after trying `/usr/local/sbin`, `/usr/local/bin`,
///    `/bin` and `/usr/bin` (issue #515).
///
/// Each candidate must be a regular file the caller may read, as when the
/// whole file was read here.
pub(super) fn open_executable(path: &str) -> Result<VfsFile, FsError> {
    let id = Id::current();
    match VfsFile::abi(id, path) {
        Ok(file) => return Ok(file),
        Err(FsError::NotFound) => {}
        Err(error) => return Err(error),
    }
    if let Some(program) = system_bin_path(path) {
        match VfsFile::abi(id, &program) {
            Ok(file) => return Ok(file),
            Err(FsError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    if applet_name(path).is_some() {
        if let Ok(busybox) = VfsFile::abi(id, fhs::bin::BUSYBOX) {
            return Ok(busybox);
        }
    }
    // The basename on its own, through every step above: a program at the
    // image root still outranks the BusyBox alias. `base` has no `/`, so this
    // recurses at most once.
    let base = path.rsplit('/').next().unwrap_or(path);
    if base != path && !base.is_empty() {
        open_executable(base)
    } else {
        Err(FsError::NotFound)
    }
}

/// The file [`load_executable`] reads for `path`, as an absolute path: the
/// file itself, the `/system/bin` program an applet-shaped name stands for, or
/// BusyBox for an applet alias. This is what `/proc/self/exe` names.
pub(super) fn real_exe_path(path: &str) -> String {
    let absolute = format!("/{}", path.trim_start_matches('/'));
    let exists = |candidate: &str| crate::fs::abi_stat(Id::current(), candidate).is_ok();
    if exists(&absolute) {
        return absolute;
    }
    let base = absolute.rsplit('/').next().unwrap_or("");
    for candidate in [system_bin_path(&absolute), system_bin_path(base)]
        .into_iter()
        .flatten()
    {
        if exists(&candidate) {
            return candidate;
        }
    }
    String::from(fhs::bin::BUSYBOX)
}

/// `/proc/self/exe` of the caller: the program `execve` recorded, or BusyBox
/// for a task the kernel started without one (the shell).
pub(super) fn self_exe() -> String {
    match task::linuxstate::exe() {
        Some(path) => String::from(&*path),
        None => String::from(fhs::bin::BUSYBOX),
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
    let entries = match crate::fs::abi_readdir(Id::current(), path) {
        Ok(entries) => entries,
        Err(FsError::NotFound) => {
            // A synthetic directory (`/etc`, `/proc`) lists what it fabricates.
            let names = super::procfs::children(path).ok_or(FsError::NotFound)?;
            for (index, (name, dir)) in names.iter().enumerate() {
                let kind = if *dir { FileKind::Dir } else { FileKind::File };
                push_dirent(&mut out, 100 + index as u64, dtype_of(kind), name);
            }
            return Ok(out);
        }
        Err(error) => return Err(error),
    };
    for entry in entries {
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
fn open_file_fd(id: Id, path: &str, meta: Meta, mode: Access, created: bool) -> u64 {
    if crate::fs::abi_persistent(path) {
        // The durable volume is read and written in place, never snapshotted.
        // Nothing reads the file here, so the read permission a snapshot open
        // gets for free from loading it has to be checked explicitly (write
        // permission already was, by the caller). A file this very open just
        // created is the caller's whatever mode it was given (`O_CREAT` with
        // 0o200 and `O_RDWR` must succeed), so the check is skipped for it.
        if mode.read && !created {
            if let Err(error) = crate::fs::abi_check(id, path, vfs::READ) {
                return fs_err(error);
            }
        }
        return open_vfs_fd(path, mode.read, mode.write, mode.append);
    }
    let (writable, append) = (mode.write, mode.append);
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
        "/dev/tty" => return super::tty::open_tty(),
        "/dev/console" | "/dev/tty0" | "/dev/tty1" => {
            return super::fd::fd_result(task::fd_open(Fd::Terminal));
        }
        "/dev/ptmx" | "/dev/pts/ptmx" => return super::tty::open_ptmx(),
        _ if DEVICES.contains(&path) => return open_device_fd(path),
        _ => {}
    }
    if let Some(index) = path.strip_prefix("/dev/pts/") {
        return super::tty::open_pts(index, flags);
    }

    let id = Id::current();
    let write_access = flags & O_ACCMODE != 0;
    let create = flags & O_CREAT != 0;
    let exclusive = flags & O_EXCL != 0;
    let truncate = flags & O_TRUNC != 0;
    let access = Access::from_flags(flags);
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
            } else if let Some(data) = super::procfs::contents(path) {
                open_snapshot(data, file_meta(meta, String::from(path), false, false))
            } else {
                open_file_fd(id, path, meta, Access::READ_ONLY, false)
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
        return open_file_fd(id, path, meta, access, false);
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
        Ok(Target::Node(meta)) => open_file_fd(id, path, meta, access, true),
        _ => err(ENOENT),
    }
}

pub(super) fn sys_openat(dirfd: u64, path: u64, flags: u64, mode: u64) -> u64 {
    match user_path(dirfd, path) {
        Ok(path) => open_path(&path, flags, mode),
        Err(code) => code,
    }
}

// `mkdir`/`rmdir`/`unlink`/`rename`/`access`/`umask`/`readlink` live in
// `pathops`, split out purely to stay under the file size limit; relative
// paths are resolved by `cwd::resolve_at` before any of them see a name.
