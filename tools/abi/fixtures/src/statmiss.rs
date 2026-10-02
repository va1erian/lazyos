//! `statmiss` — a path that names nothing must stat as `ENOENT` through every
//! stat entry point: `statx` (what Rust's `std::fs::metadata` tries first),
//! `newfstatat` and plain `stat`. A Doom build once saw `metadata("/nope")`
//! succeed because the kernel took any short name at `/` or in a directory
//! whose path merely contained `bin` for a BusyBox applet alias.
//!
//! Absolute and relative misses are tried from `/` and from a nested working
//! directory shaped like an installed app's (`.../bin`), all on `/tmp` so the
//! row needs no data disk. The raw syscalls go through `syscall(2)` so the
//! number under test is exactly the one named.

mod common;

use std::ffi::{c_char, c_int, c_long, CString};
use std::fs;

const NAME: &str = "statmiss";
const NESTED: &str = "/tmp/statmiss/apps/org.lazy.doom/1.0/bin";
const AT_FDCWD: c_long = -100;
const STATX_BASIC_STATS: c_long = 0x7ff;
const SYS_STAT: c_long = 4;
const SYS_NEWFSTATAT: c_long = 262;
const SYS_STATX: c_long = 332;
const ENOENT: c_int = 2;

extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
    fn __errno_location() -> *mut c_int;
}

/// The errno of the last failed call.
fn errno() -> c_int {
    // SAFETY: musl's `__errno_location` returns this thread's errno slot.
    unsafe { *__errno_location() }
}

/// `Ok(())` when `ret` is `-1` with `ENOENT`, otherwise what happened.
fn expect_enoent(call: &str, path: &str, ret: c_long) -> Result<(), String> {
    match (ret, errno()) {
        (-1, ENOENT) => Ok(()),
        (-1, other) => Err(format!("{call}({path}) errno {other}, want ENOENT")),
        (ret, _) => Err(format!("{call}({path}) returned {ret} for a missing path")),
    }
}

/// Stat `path` through `statx`, `newfstatat`, `stat` and `std::fs::metadata`,
/// each of which must miss.
fn all_miss(path: &str) -> Result<(), String> {
    let c_path = CString::new(path).unwrap();
    let p: *const c_char = c_path.as_ptr();
    let mut buf = [0u8; 256];
    let out = buf.as_mut_ptr();
    // SAFETY: `p` is NUL-terminated and `out` is 256 writable bytes, enough for
    // both `struct statx` (256) and `struct stat` (144).
    let ret = unsafe { syscall(SYS_STATX, AT_FDCWD, p, 0 as c_long, STATX_BASIC_STATS, out) };
    expect_enoent("statx", path, ret)?;
    // SAFETY: as above.
    let ret = unsafe { syscall(SYS_NEWFSTATAT, AT_FDCWD, p, out, 0 as c_long) };
    expect_enoent("newfstatat", path, ret)?;
    // SAFETY: as above.
    let ret = unsafe { syscall(SYS_STAT, p, out) };
    expect_enoent("stat", path, ret)?;
    match fs::metadata(path) {
        Err(error) if error.raw_os_error() == Some(ENOENT) => Ok(()),
        Err(error) => Err(format!("metadata({path}): {error}, want ENOENT")),
        Ok(_) => Err(format!("metadata({path}) succeeded for a missing path")),
    }
}

/// The misses tried from whatever the working directory is.
const MISSES: &[&str] = &[
    "/nope",
    "/nope.wad",
    "/tmp/statmiss/nope",
    "/tmp/statmiss/apps/org.lazy.doom/1.0/bin/nope",
    "/data/apps/org.lazy.doom/1.0/bin/nope",
    "nope",
    "doom1",
    "nope.wad",
    "./nope",
    "../nope",
];

fn run() -> Result<(), String> {
    fs::create_dir_all(NESTED).map_err(|error| format!("mkdir {NESTED}: {error}"))?;
    for cwd in ["/", NESTED] {
        std::env::set_current_dir(cwd).map_err(|e| format!("chdir {cwd}: {e}"))?;
        for path in MISSES {
            all_miss(path).map_err(|e| format!("cwd {cwd}: {e}"))?;
        }
    }
    // A plain name in a `bin` directory can be created, and then is found.
    fs::write("real", b"x").map_err(|e| format!("write {NESTED}/real: {e}"))?;
    let real = fs::metadata("real").map_err(|e| format!("metadata(real): {e}"))?;
    if real.len() != 1 {
        return Err(format!("metadata(real) size {}", real.len()));
    }
    fs::metadata("/tmp").map_err(|e| format!("metadata(/tmp): {e}"))?;
    Ok(())
}

fn main() {
    match run() {
        Ok(()) => common::pass(NAME),
        Err(reason) => common::fail(NAME, &reason),
    }
}
