//! `statxio` — the calls programs use to inspect the filesystem (issue #348):
//! `statx`, `preadv`/`pwritev`, the legacy `getdents`, and the synthetic
//! `/proc/mounts` that `df` and `mount` read. Everything happens on `/tmp`,
//! so the row needs no data disk.
//!
//! musl's `statx`, `preadv` and `pwritev` wrappers are the real syscalls, and
//! `getdents` (which musl itself never uses) goes through `syscall(2)`.

mod common;

use std::ffi::{c_char, c_int, c_long, c_void, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;

const NAME: &str = "statxio";
const DIR: &str = "/tmp/statxio";
const AT_FDCWD: c_int = -100;
const AT_EMPTY_PATH: c_int = 0x1000;
const STATX_BASIC_STATS: u32 = 0x7ff;
const STATX_TYPE_MODE_SIZE: u32 = 0x1 | 0x2 | 0x200;
const SYS_GETDENTS: c_long = 78;
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;

#[repr(C)]
struct IoVec {
    base: *mut c_void,
    len: usize,
}

extern "C" {
    fn statx(dirfd: c_int, path: *const c_char, flags: c_int, mask: u32, buf: *mut u8) -> c_int;
    fn preadv(fd: c_int, iov: *const IoVec, count: c_int, offset: i64) -> isize;
    fn pwritev(fd: c_int, iov: *const IoVec, count: c_int, offset: i64) -> isize;
    fn syscall(number: c_long, ...) -> c_long;
}

fn check(ok: bool, what: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(what.to_string())
    }
}

fn io<T>(result: std::io::Result<T>, what: &str) -> Result<T, String> {
    result.map_err(|error| format!("{what}: {error}"))
}

/// The fields of a `struct statx` this fixture looks at.
struct Statx {
    mask: u32,
    mode: u32,
    size: u64,
}

fn parse(buf: &[u8; 256]) -> Statx {
    Statx {
        mask: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
        mode: u32::from(u16::from_le_bytes(buf[28..30].try_into().unwrap())),
        size: u64::from_le_bytes(buf[40..48].try_into().unwrap()),
    }
}

/// `statx` of `path` (or, with `AT_EMPTY_PATH` and `path == ""`, of `dirfd`).
fn do_statx(dirfd: c_int, path: &str, flags: c_int) -> Result<Statx, String> {
    let path = CString::new(path).unwrap();
    let mut buf = [0u8; 256];
    // SAFETY: `path` is NUL-terminated and `buf` is the 256 bytes `statx` fills.
    let ret = unsafe { statx(dirfd, path.as_ptr(), flags, STATX_BASIC_STATS, buf.as_mut_ptr()) };
    check(ret == 0, "statx failed")?;
    Ok(parse(&buf))
}

fn stat_and_statx(file: &File, path: &str, len: u64) -> Result<(), String> {
    let by_path = do_statx(AT_FDCWD, path, 0)?;
    check(by_path.size == len, "statx size by path")?;
    check(by_path.mode & S_IFMT == S_IFREG, "statx mode by path")?;
    check(
        by_path.mask & STATX_TYPE_MODE_SIZE == STATX_TYPE_MODE_SIZE,
        "statx mask lacks type, mode or size",
    )?;
    let by_fd = do_statx(file.as_raw_fd(), "", AT_EMPTY_PATH)?;
    check(by_fd.size == len, "statx size by fd (AT_EMPTY_PATH)")?;
    // `std::fs::metadata` is statx underneath on Linux.
    check(io(fs::metadata(path), "metadata")?.len() == len, "std metadata size")?;
    let missing = CString::new("/tmp/statxio/none").unwrap();
    let mut buf = [0u8; 256];
    // SAFETY: as in `do_statx`.
    let ret = unsafe { statx(AT_FDCWD, missing.as_ptr(), 0, STATX_BASIC_STATS, buf.as_mut_ptr()) };
    check(ret == -1, "statx of a missing file succeeded")
}

fn vectored(file: &mut File) -> Result<(), String> {
    let (mut a, mut b) = (*b"AAAA", *b"BBBBBB");
    let out = [
        IoVec { base: a.as_mut_ptr().cast(), len: a.len() },
        IoVec { base: b.as_mut_ptr().cast(), len: b.len() },
    ];
    // SAFETY: both segments point at live, readable buffers of their length.
    let wrote = unsafe { pwritev(file.as_raw_fd(), out.as_ptr(), 2, 4) };
    check(wrote == 10, "pwritev did not write 10 bytes")?;
    check(io(file.stream_position(), "tell")? == 0, "pwritev moved the file position")?;

    let (mut x, mut y) = ([0u8; 6], [0u8; 8]);
    let inn = [
        IoVec { base: x.as_mut_ptr().cast(), len: x.len() },
        IoVec { base: y.as_mut_ptr().cast(), len: y.len() },
    ];
    // SAFETY: both segments point at live, writable buffers of their length.
    let got = unsafe { preadv(file.as_raw_fd(), inn.as_ptr(), 2, 2) };
    check(got == 12, "preadv did not read 12 bytes")?;
    check(&x == b"..AAAA" && &y[..6] == b"BBBBBB", "preadv returned other bytes")?;
    check(io(file.stream_position(), "tell")? == 0, "preadv moved the file position")?;
    // A hostile count is refused, not looped over.
    // SAFETY: the count is refused before any segment is read.
    let refused = unsafe { preadv(file.as_raw_fd(), inn.as_ptr(), 1 << 20, 0) };
    check(refused == -1, "preadv accepted a million segments")
}

/// Names in a directory as the legacy `getdents` reports them: each record is
/// `ino u64 | off u64 | reclen u16 | name\0 | padding | type u8`.
fn legacy_names(dir: &File) -> Result<Vec<String>, String> {
    let mut buf = [0u8; 1024];
    // SAFETY: `buf` is the writable 1 KiB the call is told about.
    let got = unsafe { syscall(SYS_GETDENTS, dir.as_raw_fd() as c_long, buf.as_mut_ptr(), buf.len()) };
    check(got > 0, "getdents returned no entries")?;
    let (mut names, mut at) = (Vec::new(), 0usize);
    while at < got as usize {
        let reclen = usize::from(u16::from_le_bytes(buf[at + 16..at + 18].try_into().unwrap()));
        check(reclen >= 24 && at + reclen <= got as usize, "a bad getdents record length")?;
        let name = &buf[at + 18..at + reclen];
        let end = name.iter().position(|b| *b == 0).ok_or("an unterminated name")?;
        names.push(String::from_utf8_lossy(&name[..end]).into_owned());
        at += reclen;
    }
    Ok(names)
}

fn directory() -> Result<(), String> {
    let dir = io(File::open(DIR), "open dir")?;
    let mut names = legacy_names(&dir)?;
    names.sort();
    check(names == [".", "..", "data.bin", "second"], &format!("getdents listed {names:?}"))
}

fn mounts() -> Result<(), String> {
    let mut text = String::new();
    io(io(File::open("/proc/mounts"), "open /proc/mounts")?.read_to_string(&mut text), "read")?;
    let mounted: Vec<&str> = text.lines().filter_map(|line| line.split(' ').nth(1)).collect();
    check(mounted.contains(&"/") && mounted.contains(&"/tmp"), "/proc/mounts lacks / or /tmp")?;
    check(
        text.lines().all(|line| line.split(' ').count() == 6),
        "a /proc/mounts line is not six columns",
    )
}

fn run() -> Result<(), String> {
    let _ = fs::remove_dir_all(DIR);
    io(fs::create_dir(DIR), "mkdir")?;
    let path = format!("{DIR}/data.bin");
    let mut file = io(
        OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path),
        "create",
    )?;
    io(file.write_all(b"......"), "write")?;
    io(file.seek(SeekFrom::Start(0)), "seek")?;
    io(fs::write(format!("{DIR}/second"), b"2"), "second file")?;

    vectored(&mut file)?;
    stat_and_statx(&file, &path, 14)?;
    directory()?;
    mounts()?;
    // Close first: a file unlinked while open is parked as a hidden `.unlinked-N`
    // entry until its last close (`fs/openfile.rs`), and /tmp opens read through
    // since #265, so `rmdir` of the directory would still see it.
    drop(file);
    io(fs::remove_dir_all(DIR), "cleanup")
}

fn main() {
    match run() {
        Ok(()) => common::pass(NAME),
        Err(reason) => common::fail(NAME, &reason),
    }
}
