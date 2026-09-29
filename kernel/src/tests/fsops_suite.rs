//! The native filesystem syscalls (15-20) and `power` (21) behind the shell's
//! `dir`/`copy`/`del`/`ren`/`mkdir`/`reboot`/`shutdown` (issue #6).

use super::*;
use crate::ipc::credentials::{self, Cred};
use crate::process::power;

const ENOENT: i64 = 2;
const EPERM: i64 = 1;
const EFAULT: i64 = 14;
const EEXIST: i64 = 17;
const EINVAL: i64 = 22;
const ENAMETOOLONG: i64 = 36;
const ENOSPC: i64 = 28;

const STAT: u64 = 15;
const READDIR: u64 = 16;
const WRITE_FILE: u64 = 17;
const MKDIR: u64 = 18;
const UNLINK: u64 = 19;
const RENAME: u64 = 20;
const POWER: u64 = 21;

fn failed(errno: i64) -> u64 {
    errno.wrapping_neg() as u64
}

/// A NUL-terminated copy of `text`, as a user path argument.
fn cstr(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

fn call(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    process::dispatch_for_test(nr, a1, a2, a3)
}

/// `nr` on one path.
fn path_call(nr: u64, path: &str) -> u64 {
    let path = cstr(path);
    call(nr, path.as_ptr() as u64, 0, 0)
}

fn put(path: &str, data: &[u8]) -> u64 {
    let path = cstr(path);
    call(
        WRITE_FILE,
        path.as_ptr() as u64,
        data.as_ptr() as u64,
        data.len() as u64,
    )
}

fn slurp(path: &str) -> Option<Vec<u8>> {
    let path = cstr(path);
    let mut buf = vec![0u8; 4096];
    let n = call(
        3,
        path.as_ptr() as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    );
    (n != u64::MAX).then(|| buf[..n as usize].to_vec())
}

fn stat(path: &str) -> Result<(u64, u64), u64> {
    let path = cstr(path);
    let mut out = [0u64; 2];
    match call(STAT, path.as_ptr() as u64, out.as_mut_ptr() as u64, 0) {
        0 => Ok((out[0], out[1])),
        code => Err(code),
    }
}

fn list(path: &str) -> Result<String, u64> {
    let path = cstr(path);
    let mut buf = vec![0u8; 2048];
    let n = call(
        READDIR,
        path.as_ptr() as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
    );
    if n > buf.len() as u64 {
        return Err(n);
    }
    Ok(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::reset_for_task(task::KERNEL_TASK);
    crate::fs::init();
    Ok(())
}

/// Removes the listed paths on drop so a failed check leaves `/tmp` clean.
struct Cleanup(&'static [&'static str]);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for path in self.0 {
            let _ = path_call(UNLINK, path);
        }
    }
}

/// Runs `f` with pointer validation on, so a kernel address is a `-EFAULT`.
fn strict<R>(f: impl FnOnce() -> R) -> R {
    let previous = crate::user_ptr::set_trust_kernel_pointers(false);
    let result = f();
    crate::user_ptr::set_trust_kernel_pointers(previous);
    result
}

/// mkdir, write, read back, stat, list, rename, delete: the whole shell
/// vocabulary against `/tmp`.
pub fn shell_file_lifecycle() -> Result<(), String> {
    fresh()?;
    let _clean = Cleanup(&["/tmp/fsops/a.txt", "/tmp/fsops/b.txt", "/tmp/fsops"]);
    check!(path_call(MKDIR, "/tmp/fsops") == 0, "mkdir failed");
    check!(
        path_call(MKDIR, "/tmp/fsops") == failed(EEXIST),
        "a second mkdir was not EEXIST"
    );
    check!(
        put("/tmp/fsops/a.txt", b"hello") == 5,
        "write_file did not report 5 bytes"
    );
    check!(
        slurp("/tmp/fsops/a.txt").as_deref() == Some(&b"hello"[..]),
        "read back differs"
    );

    // Replacing shrinks: write_file truncates first.
    check!(put("/tmp/fsops/a.txt", b"hi") == 2, "overwrite failed");
    check!(
        slurp("/tmp/fsops/a.txt").as_deref() == Some(&b"hi"[..]),
        "overwrite left old bytes"
    );
    check!(
        stat("/tmp/fsops/a.txt") == Ok((2, 0)),
        "stat of a file: {:?}",
        stat("/tmp/fsops/a.txt")
    );
    check!(
        stat("/tmp/fsops").map(|s| s.1) == Ok(1),
        "stat of a directory is not kind 1"
    );
    check!(
        list("/tmp/fsops") == Ok("f 2 a.txt\n".into()),
        "listing: {:?}",
        list("/tmp/fsops")
    );

    check!(
        call(
            RENAME,
            cstr("/tmp/fsops/a.txt").as_ptr() as u64,
            cstr("/tmp/fsops/b.txt").as_ptr() as u64,
            0
        ) == 0,
        "rename failed"
    );
    check!(
        stat("/tmp/fsops/a.txt") == Err(failed(ENOENT)),
        "old name survived the rename"
    );
    check!(
        slurp("/tmp/fsops/b.txt").as_deref() == Some(&b"hi"[..]),
        "rename lost the data"
    );

    check!(
        path_call(UNLINK, "/tmp/fsops/b.txt") == 0,
        "unlink of a file failed"
    );
    check!(
        path_call(UNLINK, "/tmp/fsops") == 0,
        "unlink of an empty directory failed"
    );
    check!(
        stat("/tmp/fsops") == Err(failed(ENOENT)),
        "directory survived its removal"
    );
    Ok(())
}

/// Bad paths and hostile arguments are reported, never a panic or a fault.
pub fn errors_are_reported() -> Result<(), String> {
    fresh()?;
    check!(
        stat("/tmp/nope") == Err(failed(ENOENT)),
        "stat of a missing file"
    );
    check!(
        path_call(UNLINK, "/tmp/nope") == failed(ENOENT),
        "unlink of a missing file"
    );
    check!(
        path_call(STAT, "") == failed(EINVAL),
        "an empty path was accepted"
    );
    check!(
        list("/tmp/nope") == Err(failed(ENOENT)),
        "listing a missing directory"
    );
    check!(
        call(
            RENAME,
            cstr("/tmp/nope").as_ptr() as u64,
            cstr("/tmp/x").as_ptr() as u64,
            0
        ) == failed(ENOENT),
        "rename of a missing file"
    );
    check!(
        put("/tmp/nodir/x", b"x") == failed(ENOENT),
        "write into a missing directory"
    );
    let huge = process::fsops::MAX_WRITE + 1;
    let path = cstr("/tmp/huge");
    check!(
        call(WRITE_FILE, path.as_ptr() as u64, path.as_ptr() as u64, huge) == failed(ENOSPC),
        "an oversized write was accepted"
    );
    check!(
        stat("/tmp/huge") == Err(failed(ENOENT)),
        "an oversized write left a file"
    );
    // 23 is the device syscall (issue #240), 24 the wall clock (issue #369);
    // 25 is the first free number.
    check!(
        call(25, 0, 0, 0) == u64::MAX,
        "an unknown syscall number was accepted"
    );
    // A path with no terminator inside the limit is refused, not truncated.
    let unterminated = vec![b'a'; 2000];
    check!(
        call(STAT, unterminated.as_ptr() as u64, 0, 0) == failed(ENAMETOOLONG),
        "an unterminated path was accepted"
    );
    // The FAT boot volume stays read-only.
    if crate::fs::init() {
        check!(
            path_call(MKDIR, "/NEWDIR") != 0,
            "mkdir succeeded on the read-only boot volume"
        );
        check!(
            put("/HELLO.TXT", b"x") != 5,
            "overwrote a file on the read-only boot volume"
        );
    }
    Ok(())
}

/// `readdir` writes whole lines only; a short buffer truncates the listing.
pub fn readdir_truncates_on_line_boundaries() -> Result<(), String> {
    fresh()?;
    let _clean = Cleanup(&["/tmp/rd/one", "/tmp/rd/two", "/tmp/rd"]);
    check!(path_call(MKDIR, "/tmp/rd") == 0, "mkdir");
    check!(
        put("/tmp/rd/one", b"1") == 1 && put("/tmp/rd/two", b"22") == 2,
        "seed files"
    );
    let path = cstr("/tmp/rd");
    for cap in 0..=40u64 {
        let mut buf = vec![b'#'; 64];
        let n = call(READDIR, path.as_ptr() as u64, buf.as_mut_ptr() as u64, cap);
        check!(n <= cap, "cap {cap}: wrote {n} bytes");
        let text = &buf[..n as usize];
        check!(
            text.is_empty() || text.ends_with(b"\n"),
            "cap {cap}: a partial line was written"
        );
        check!(
            buf[n as usize..].iter().all(|b| *b == b'#'),
            "cap {cap}: wrote past the reported length"
        );
    }
    Ok(())
}

/// Kernel addresses passed as paths or buffers are `-EFAULT`; a bad data
/// pointer never truncates an existing file.
pub fn kernel_pointers_are_refused() -> Result<(), String> {
    fresh()?;
    let _clean = Cleanup(&["/tmp/keep"]);
    check!(put("/tmp/keep", b"precious") == 8, "seed file");
    let path = cstr("/tmp/keep");
    let secret = vec![0xA5u8; 64];
    let mut canary = vec![0xA5u8; 64];
    strict(|| -> Result<(), String> {
        check!(
            call(STAT, secret.as_ptr() as u64, canary.as_mut_ptr() as u64, 0) == failed(EFAULT),
            "stat took a kernel path"
        );
        check!(
            call(STAT, path.as_ptr() as u64, canary.as_mut_ptr() as u64, 0) == failed(EFAULT),
            "stat wrote to a kernel buffer"
        );
        check!(
            call(
                READDIR,
                cstr("/tmp").as_ptr() as u64,
                canary.as_mut_ptr() as u64,
                64
            ) == failed(EFAULT),
            "readdir wrote to a kernel buffer"
        );
        check!(
            call(WRITE_FILE, path.as_ptr() as u64, secret.as_ptr() as u64, 8) == failed(EFAULT),
            "write_file read kernel memory"
        );
        for nr in [MKDIR, UNLINK] {
            check!(
                call(nr, secret.as_ptr() as u64, 0, 0) == failed(EFAULT),
                "syscall {nr} took a kernel path"
            );
        }
        Ok(())
    })?;
    check!(
        canary.iter().all(|b| *b == 0xA5),
        "a syscall wrote through a kernel pointer"
    );
    check!(
        slurp("/tmp/keep").as_deref() == Some(&b"precious"[..]),
        "a refused write_file damaged the existing file"
    );
    Ok(())
}

/// Only `CAP_SYS_ADMIN` may stop the machine, and the gate runs before the op
/// is decoded. The permitted path is not run: it would end the test VM.
pub fn power_is_capability_gated() -> Result<(), String> {
    fresh()?;
    let me = task::current();
    let guest = Cred::new(1000, 1000, 0, 0, 7);
    credentials::set(me, guest);
    let reboot = call(POWER, power::REBOOT, 0, 0);
    let shutdown = call(POWER, power::SHUTDOWN, 0, 0);
    let bogus = call(POWER, 99, 0, 0);
    credentials::reset_for_task(me);
    check!(
        reboot == failed(EPERM),
        "an unprivileged reboot -> {reboot:#x}"
    );
    check!(
        shutdown == failed(EPERM),
        "an unprivileged shutdown -> {shutdown:#x}"
    );
    check!(
        bogus == failed(EPERM),
        "the gate leaked which ops exist: {bogus:#x}"
    );
    check!(
        power::authorize(power::REBOOT) == Ok(power::REBOOT),
        "root was refused a reboot"
    );
    check!(
        power::authorize(99) == Err(power::Refusal::BadOp),
        "root's bogus op was accepted"
    );
    Ok(())
}

/// Soak: thousands of create/rename/delete generations leak neither frames
/// nor directory entries.
pub fn soak_file_churn() -> Result<(), String> {
    fresh()?;
    let _clean = Cleanup(&["/tmp/churn"]);
    check!(path_call(MKDIR, "/tmp/churn") == 0, "mkdir");
    let before = mem::frame_stats().live();
    for round in 0..3000u32 {
        let name = format!("/tmp/churn/f{}", round % 7);
        let moved = format!("/tmp/churn/g{}", round % 7);
        let body = vec![round as u8; (round % 300) as usize];
        check!(
            put(&name, &body) == body.len() as u64,
            "round {round}: write"
        );
        check!(
            slurp(&name).as_deref() == Some(&body[..]),
            "round {round}: read back"
        );
        let (from, to) = (cstr(&name), cstr(&moved));
        check!(
            call(RENAME, from.as_ptr() as u64, to.as_ptr() as u64, 0) == 0,
            "round {round}: rename"
        );
        check!(path_call(UNLINK, &moved) == 0, "round {round}: unlink");
    }
    check!(
        list("/tmp/churn") == Ok(String::new()),
        "entries leaked: {:?}",
        list("/tmp/churn")
    );
    check!(path_call(UNLINK, "/tmp/churn") == 0, "rmdir");
    let after = mem::frame_stats().live();
    check!(after <= before + 8, "frames leaked: {before} -> {after}");
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("fsops_shell_file_lifecycle", shell_file_lifecycle),
    ("fsops_errors_are_reported", errors_are_reported),
    (
        "fsops_readdir_line_boundaries",
        readdir_truncates_on_line_boundaries,
    ),
    ("fsops_kernel_pointers_refused", kernel_pointers_are_refused),
    ("fsops_power_capability_gated", power_is_capability_gated),
    ("fsops_soak_file_churn", soak_file_churn),
];
