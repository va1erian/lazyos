//! A path that names nothing stats as `ENOENT` through `statx`, `newfstatat`
//! and `stat`, absolute or relative, from `/` or from a nested `.../bin`
//! working directory. The resolver used to take any plain name at `/` or in a
//! directory whose path merely contained `bin` for a BusyBox applet alias, so
//! `std::fs::metadata("/nope")` succeeded (and `open(O_CREAT)` of `bin/real`
//! in an app directory was refused as read-only). The aliases still answer in
//! the real `$PATH` directories.

use super::*;
use crate::fs::vfs::{FsError, Id};
use crate::ipc::credentials::{self, Cred};

const SYS_CLOSE: u64 = 3;
const SYS_STAT: u64 = 4;
const SYS_OPENAT: u64 = 257;
const SYS_NEWFSTATAT: u64 = 262;
const SYS_STATX: u64 = 332;
const AT_FDCWD: u64 = (-100i64) as u64;
const STATX_BASIC_STATS: u64 = 0x7ff;
const O_WRONLY_CREAT: u64 = 0o1 | 0o100;
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;

/// An app's own `bin` directory, shaped like an installed package's.
const APP_BIN: &str = "/tmp/apps/org.lazy.doom/1.0/bin";

/// Paths that name nothing, whatever the working directory.
const MISSES: &[&str] = &[
    "/nope",
    "/nope.wad",
    "/ls",
    "/tmp/nope",
    "/tmp/apps/org.lazy.doom/1.0/bin/nope",
    "/tmp/apps/org.lazy.doom/1.0/bin/ls",
    "/cabinet/ls",
    "nope",
    "ls",
    "nope.wad",
    "./nope",
    "../nope",
];

/// Applet aliases that must still resolve, through every stat entry point.
const APPLETS: &[&str] = &["/bin/ls", "/sbin/ls", "/usr/bin/ls", "/usr/local/bin/rhai"];

fn fs_error(error: FsError) -> String {
    String::from(error.message())
}

/// Root over a fresh ramfs ABI table with a BusyBox to alias and the app's
/// `bin` directory; the working directory is `/`.
fn setup() -> Result<(), String> {
    fresh()?;
    crate::fs::install_abi_ramfs_for_test();
    credentials::set(task::current(), Cred::ROOT);
    let id = Id::current();
    crate::fs::abi_create(id, fhs::boot::BUSYBOX_PATH, 0o755).map_err(fs_error)?;
    crate::fs::abi_write(id, fhs::boot::BUSYBOX_PATH, 0, b"\x7fELF busybox").map_err(fs_error)?;
    let mut dir = String::new();
    for part in APP_BIN.split('/').filter(|part| !part.is_empty()) {
        dir.push('/');
        dir.push_str(part);
        match crate::fs::abi_mkdir(id, &dir, 0o755) {
            Ok(_) | Err(FsError::Exists) => {}
            Err(error) => return Err(format!("mkdir {dir}: {}", error.message())),
        }
    }
    task::set_cwd("/");
    Ok(())
}

fn c(text: &str) -> Vec<u8> {
    let mut out = Vec::from(text.as_bytes());
    out.push(0);
    out
}

/// What `statx`, `newfstatat` and `stat` return for `path`, and the
/// `st_mode`/`stx_mode` each reported (zero when it wrote nothing).
fn stat_all(path: &str) -> [(u64, u32); 3] {
    let path = c(path);
    let p = path.as_ptr() as u64;
    let mut statx = [0u8; 256];
    let ret_x = process::linux::dispatch_args5_for_test(
        SYS_STATX,
        AT_FDCWD,
        p,
        0,
        STATX_BASIC_STATS,
        statx.as_mut_ptr() as u64,
    );
    let mode_x = u32::from(u16::from_le_bytes([statx[28], statx[29]]));
    let mut at = [0u8; 144];
    let ret_at = process::linux::dispatch_args_for_test(
        SYS_NEWFSTATAT,
        AT_FDCWD,
        p,
        at.as_mut_ptr() as u64,
        0,
    );
    let mut plain = [0u8; 144];
    let ret_plain = process::linux::dispatch_for_test(SYS_STAT, p, plain.as_mut_ptr() as u64, 0);
    let st_mode = |buf: &[u8; 144]| u32::from_le_bytes(buf[24..28].try_into().unwrap());
    [
        (ret_x, mode_x),
        (ret_at, st_mode(&at)),
        (ret_plain, st_mode(&plain)),
    ]
}

const CALLS: [&str; 3] = ["statx", "newfstatat", "stat"];

/// Every stat entry point misses `path` and writes nothing.
fn expect_missing(cwd: &str, path: &str) -> Result<(), String> {
    for (call, (ret, mode)) in CALLS.iter().zip(stat_all(path)) {
        check!(
            ret == ENOENT,
            "cwd {cwd}: {call}({path}) returned {ret:#x}, want ENOENT"
        );
        check!(mode == 0, "cwd {cwd}: {call}({path}) wrote mode {mode:#o}");
    }
    Ok(())
}

/// Every stat entry point finds a regular file at `path`.
fn expect_file(path: &str) -> Result<(), String> {
    for (call, (ret, mode)) in CALLS.iter().zip(stat_all(path)) {
        check!(ret == 0, "{call}({path}) returned {ret:#x}");
        check!(
            mode & S_IFMT == S_IFREG,
            "{call}({path}) reported mode {mode:#o}"
        );
    }
    Ok(())
}

pub(super) fn stat_missing_paths_are_enoent() -> Result<(), String> {
    setup()?;
    for cwd in ["/", APP_BIN] {
        task::set_cwd(cwd);
        for path in MISSES {
            expect_missing(cwd, path)?;
        }
    }
    task::set_cwd("/");
    Ok(())
}

pub(super) fn stat_applet_alias_only_in_path_dirs() -> Result<(), String> {
    setup()?;
    for path in APPLETS {
        expect_file(path)?;
    }
    // Relative names reach an alias only from a `$PATH` directory's own cwd.
    task::set_cwd("/bin");
    expect_file("ls")?;
    task::set_cwd(APP_BIN);
    expect_missing(APP_BIN, "ls")?;
    // A plain name in an app's `bin` is an ordinary file it can create.
    let name = c("real");
    let fd = process::linux::dispatch_args_for_test(
        SYS_OPENAT,
        AT_FDCWD,
        name.as_ptr() as u64,
        O_WRONLY_CREAT,
        0o644,
    );
    check!(
        (fd as i64) >= 0,
        "openat(O_CREAT, {APP_BIN}/real) returned {fd:#x}"
    );
    process::linux::dispatch_for_test(SYS_CLOSE, fd, 0, 0);
    expect_file("real")?;
    expect_file(&format!("{APP_BIN}/real"))?;
    task::set_cwd("/");
    check!(fds_clean(), "stat alias test left a descriptor");
    Ok(())
}

/// Soak: alternating working directories, misses and alias hits stay correct
/// and leave no frames or descriptors behind.
pub(super) fn stat_miss_soak() -> Result<(), String> {
    setup()?;
    // Warm-up absorbs one-time allocations so the steady state is compared.
    expect_missing("/", "/nope")?;
    expect_file("/bin/ls")?;
    let frames_before = mem::frame_stats().live();
    for round in 0..2000usize {
        let cwd = if round % 2 == 0 { "/" } else { APP_BIN };
        task::set_cwd(cwd);
        expect_missing(cwd, MISSES[round % MISSES.len()])
            .map_err(|e| format!("round {round}: {e}"))?;
        expect_file(APPLETS[round % APPLETS.len()]).map_err(|e| format!("round {round}: {e}"))?;
    }
    task::set_cwd("/");
    let frames_after = mem::frame_stats().live();
    check!(
        frames_after <= frames_before,
        "live frames grew from {frames_before} to {frames_after} over 2000 rounds"
    );
    check!(fds_clean(), "stat soak left a descriptor");
    Ok(())
}
