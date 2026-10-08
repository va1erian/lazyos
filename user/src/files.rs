//! Ring-3 wrappers for the native filesystem syscalls 15-22 (issue #6), 28
//! (`append_file`), 30 (`read_at`) and 32 (`chmod`).
//!
//! Paths are absolute strings; the kernel checks permissions and reports
//! failures as `-errno`, which these wrappers hand back as `Err(errno)`.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use lazyos_sys::nr::{
    APPEND_FILE as SYS_APPEND_FILE, CHMOD as SYS_CHMOD, FSYNC as SYS_FSYNC, MKDIR as SYS_MKDIR,
    POWER as SYS_POWER, READDIR as SYS_READDIR, READ_AT as SYS_READ_AT, RENAME as SYS_RENAME,
    STAT as SYS_STAT, UNLINK as SYS_UNLINK, WRITE_FILE as SYS_WRITE_FILE,
};

/// Largest file `write_file` accepts (mirrors the kernel's `MAX_WRITE`).
pub const MAX_FILE: usize = 1 << 20;
/// Bytes reserved for one directory listing.
const LIST_BUF: usize = 32 * 1024;

/// `power` op codes.
pub const REBOOT: u64 = 0;
pub const SHUTDOWN: u64 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    File,
    Dir,
}

/// One directory entry as `readdir` reports it.
pub struct Entry {
    pub kind: Kind,
    pub size: u64,
    pub name: String,
}

fn syscall(nr: u64, a: u64, b: u64, c: u64) -> i64 {
    // SAFETY: one of the filesystem syscalls; every pointer passed is valid
    // for the length passed alongside it, and the kernel validates them again
    // before use.
    unsafe { lazyos_sys::raw::syscall3(nr, a, b, c) }
}

/// `Ok(value)` for a non-negative result, `Err(errno)` for `-errno`.
fn check(code: i64) -> Result<u64, i64> {
    if code < 0 {
        Err(-code)
    } else {
        Ok(code as u64)
    }
}

fn nul_terminated(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 1);
    bytes.extend_from_slice(text.as_bytes());
    bytes.push(0);
    bytes
}

/// Size and kind of `path`.
pub fn stat(path: &str) -> Result<(u64, Kind), i64> {
    let path = nul_terminated(path);
    let mut out = [0u64; 2];
    check(syscall(
        SYS_STAT,
        path.as_ptr() as u64,
        out.as_mut_ptr() as u64,
        0,
    ))?;
    Ok((out[0], if out[1] == 1 { Kind::Dir } else { Kind::File }))
}

/// The entries of directory `path`.
pub fn list(path: &str) -> Result<Vec<Entry>, i64> {
    let path = nul_terminated(path);
    let mut buf = vec![0u8; LIST_BUF];
    let len = check(syscall(
        SYS_READDIR,
        path.as_ptr() as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    ))? as usize;
    let text = String::from_utf8_lossy(&buf[..len.min(LIST_BUF)]).into_owned();
    Ok(text.lines().filter_map(parse_entry).collect())
}

/// `"<d|f> <size> <name>"`, the kernel's listing line.
fn parse_entry(line: &str) -> Option<Entry> {
    let mut parts = line.splitn(3, ' ');
    let kind = match parts.next()? {
        "d" => Kind::Dir,
        "f" => Kind::File,
        _ => return None,
    };
    let size = parts.next()?.parse().ok()?;
    Some(Entry {
        kind,
        size,
        name: parts.next()?.into(),
    })
}

/// The whole contents of a file (at most [`MAX_FILE`] bytes).
pub fn read_all(path: &str) -> Result<Vec<u8>, i64> {
    read_up_to(path, MAX_FILE)
}

/// The whole contents of a file of at most `limit` bytes; `EFBIG` for a larger
/// one, checked before any buffer is allocated. Reading has no per-call cap
/// (`read_file` fills whatever buffer it is given), so a service that handles
/// big files, such as a package, sets its own limit.
pub fn read_up_to(path: &str, limit: usize) -> Result<Vec<u8>, i64> {
    let (size, kind) = stat(path)?;
    if kind == Kind::Dir {
        return Err(21); // EISDIR
    }
    if size as usize > limit {
        return Err(27); // EFBIG
    }
    let mut data = vec![0u8; size as usize];
    let name = nul_terminated(path);
    match crate::sys::read_file(&name, &mut data) {
        Some(n) => {
            data.truncate(n);
            Ok(data)
        }
        None => Err(2), // ENOENT
    }
}

/// Read up to `buf.len()` bytes of `path` at `offset` (native syscall 30);
/// the count read, 0 at the end of the file. The kernel serves at most
/// [`MAX_FILE`] bytes per call and never loads the whole file, so a big file is
/// read by calling this in a loop ([`read_large`]).
pub fn read_at(path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, i64> {
    let path = nul_terminated(path);
    let request = [buf.as_mut_ptr() as u64, buf.len() as u64, offset];
    check(syscall(
        SYS_READ_AT,
        path.as_ptr() as u64,
        request.as_ptr() as u64,
        0,
    ))
    .map(|read| read as usize)
}

/// Fill `data` with the whole file at `path` (at most `limit` bytes; `EFBIG`
/// for a larger one), reusing `data`'s allocation. Read in [`MAX_FILE`] ranges
/// through [`read_at`], so the kernel never holds more than one range of it,
/// whatever the file's size. A file whose size changes during the read (it
/// shrank under a range, or its size differs afterwards) is `EINVAL` and
/// leaves `data` empty. A rewrite that keeps the size cannot be seen here (the
/// native `stat` has no modification time): callers that need integrity check
/// the content, as `pkgd` does through `lazypkg`'s per-entry CRC-32.
pub fn read_large(path: &str, limit: usize, data: &mut Vec<u8>) -> Result<(), i64> {
    let (size, kind) = stat(path)?;
    if kind == Kind::Dir {
        return Err(21); // EISDIR
    }
    if size > limit as u64 {
        return Err(27); // EFBIG
    }
    data.clear();
    data.resize(size as usize, 0);
    let mut filled = 0;
    while filled < data.len() {
        let end = data.len().min(filled + MAX_FILE);
        let read =
            read_at(path, filled as u64, &mut data[filled..end]).inspect_err(|_| data.clear())?;
        if read == 0 {
            data.clear();
            return Err(22); // EINVAL: the file shrank under the read
        }
        filled += read;
    }
    if !matches!(stat(path), Ok((after, Kind::File)) if after == size) {
        data.clear();
        return Err(22); // EINVAL: the file changed size during the read
    }
    Ok(())
}

/// Create or replace `path` with `data`.
pub fn write_file(path: &str, data: &[u8]) -> Result<(), i64> {
    let path = nul_terminated(path);
    check(syscall(
        SYS_WRITE_FILE,
        path.as_ptr() as u64,
        data.as_ptr() as u64,
        data.len() as u64,
    ))
    .map(|_| ())
}

/// Append `data` (at most [`MAX_FILE`] bytes) to the end of `path`, creating it
/// when absent.
pub fn append_file(path: &str, data: &[u8]) -> Result<(), i64> {
    let path = nul_terminated(path);
    check(syscall(
        SYS_APPEND_FILE,
        path.as_ptr() as u64,
        data.as_ptr() as u64,
        data.len() as u64,
    ))
    .map(|_| ())
}

/// Create or replace `path` with `data` of any length: the first chunk is a
/// [`write_file`] (so an existing file is replaced), the rest [`append_file`]s.
/// A failure part-way leaves the bytes written so far; the caller removes the
/// file.
pub fn write_large(path: &str, data: &[u8]) -> Result<(), i64> {
    let mut chunks = data.chunks(MAX_FILE);
    write_file(path, chunks.next().unwrap_or(&[]))?;
    for chunk in chunks {
        append_file(path, chunk)?;
    }
    Ok(())
}

/// Set the permission bits of `path` to `mode` (`0o7777` bits only; anything
/// above is `Err(EINVAL)`). Only the file's owner or root may (`Err(EPERM)`),
/// the same rule as the Linux `chmod`.
pub fn chmod(path: &str, mode: u16) -> Result<(), i64> {
    let path = nul_terminated(path);
    check(syscall(SYS_CHMOD, path.as_ptr() as u64, u64::from(mode), 0)).map(|_| ())
}

pub fn mkdir(path: &str) -> Result<(), i64> {
    let path = nul_terminated(path);
    check(syscall(SYS_MKDIR, path.as_ptr() as u64, 0, 0)).map(|_| ())
}

/// Remove a file or an empty directory.
pub fn remove(path: &str) -> Result<(), i64> {
    let path = nul_terminated(path);
    check(syscall(SYS_UNLINK, path.as_ptr() as u64, 0, 0)).map(|_| ())
}

pub fn rename(from: &str, to: &str) -> Result<(), i64> {
    let (from, to) = (nul_terminated(from), nul_terminated(to));
    check(syscall(
        SYS_RENAME,
        from.as_ptr() as u64,
        to.as_ptr() as u64,
        0,
    ))
    .map(|_| ())
}

/// Flush the filesystem holding `path` to stable storage (`fsync(2)`).
///
/// The native VFS is per-mount, so this flushes every pending write on the
/// volume, not just `path`'s. `confd` calls it on its `store.tmp` before the
/// atomic rename, so the new bytes are durable before they become visible.
pub fn fsync(path: &str) -> Result<(), i64> {
    let path = nul_terminated(path);
    check(syscall(SYS_FSYNC, path.as_ptr() as u64, 0, 0)).map(|_| ())
}

/// `power` op: sync and reset the machine.
pub const POWER_REBOOT: u64 = 0;
/// `power` op: sync and power the machine off.
pub const POWER_OFF: u64 = 1;
/// `power` op: arm the kernel's shutdown watchdog (`arg` is the stop to
/// force).
const POWER_ARM_WATCHDOG: u64 = 2;

/// Reboot ([`POWER_REBOOT`]) or power off ([`POWER_OFF`]); returns only when
/// refused (`Err(EPERM)` without `CAP_SYS_ADMIN`). `init` is the only caller:
/// everyone else asks it for an orderly shutdown (docs/shutdown.md).
pub fn power(op: u64) -> Result<(), i64> {
    check(syscall(SYS_POWER, op, 0, 0)).map(|_| ())
}

/// Arm the kernel's shutdown watchdog: if the machine is still running some
/// 30 s later, the kernel kills every task, syncs and performs `stop`
/// ([`POWER_REBOOT`] or [`POWER_OFF`]) itself. One-way; `CAP_SYS_ADMIN` only.
pub fn power_arm(stop: u64) -> Result<(), i64> {
    check(syscall(SYS_POWER, POWER_ARM_WATCHDOG, stop, 0)).map(|_| ())
}

/// Human-readable text for the errnos these calls return.
pub fn describe(errno: i64) -> &'static str {
    match errno {
        1 => "operation not permitted",
        2 => "no such file or directory",
        13 => "permission denied",
        14 => "bad address",
        17 => "file exists",
        20 => "not a directory",
        21 => "is a directory",
        22 => "invalid argument",
        27 => "file too large",
        28 => "no space left",
        30 => "read-only file system",
        36 => "name too long",
        39 => "directory not empty",
        _ => "error",
    }
}
