//! Ring-3 wrappers for the native filesystem syscalls 15-21 (issue #6).
//!
//! Paths are absolute strings; the kernel checks permissions and reports
//! failures as `-errno`, which these wrappers hand back as `Err(errno)`.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;

const SYS_STAT: u64 = 15;
const SYS_READDIR: u64 = 16;
const SYS_WRITE_FILE: u64 = 17;
const SYS_MKDIR: u64 = 18;
const SYS_UNLINK: u64 = 19;
const SYS_RENAME: u64 = 20;
const SYS_POWER: u64 = 21;

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
    let code: u64;
    // SAFETY: `int 0x80` with one of the filesystem syscalls; every pointer
    // passed is valid for the length passed alongside it, and the kernel
    // validates them again before use.
    unsafe {
        asm!(
            "int 0x80",
            in("rax") nr,
            in("rdi") a,
            in("rsi") b,
            in("rdx") c,
            lateout("rax") code,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
            clobber_abi("sysv64"),
        );
    }
    code as i64
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
    let (size, kind) = stat(path)?;
    if kind == Kind::Dir {
        return Err(21); // EISDIR
    }
    if size as usize > MAX_FILE {
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

/// Reboot or shut down; returns only when refused (`Err(EPERM)` without
/// `CAP_SYS_ADMIN`).
pub fn power(op: u64) -> Result<(), i64> {
    check(syscall(SYS_POWER, op, 0, 0)).map(|_| ())
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
