//! `utimensat` and the legacy `utime`/`utimes`/`futimesat` on `/data` (issue
//! #345): who may set which timestamp, `UTIME_NOW`/`UTIME_OMIT`, malformed
//! times, and ext2's 32-bit range. Shares its helpers with `attrs.rs`.

use super::attrs::*;
use super::*;

/// `utimensat` and the legacy `utime`/`utimes`/`futimesat`: explicit times
/// need ownership, "now" also accepts write permission, `UTIME_OMIT` keeps a
/// stamp, malformed times are `EINVAL`.
pub fn utimes_rules() -> Result<(), String> {
    let data = Data::new(0)?;
    alice_file("/data/t", 0o666)?;
    let path = Some("/data/t");

    check!(
        as_user(ALICE, || utimensat(
            AT_FDCWD,
            path,
            at_times(1 << 20, 2 << 20),
            0
        )) == 0,
        "the owner's utimensat failed"
    );
    let seen = attrs("/data/t")?;
    check!(
        seen.atime == 1 << 20 && seen.mtime == 2 << 20,
        "explicit times gave {seen:?}"
    );
    let omit_atime = Some([(0, UTIME_OMIT), (3 << 20, 0)]);
    check!(
        utimensat(AT_FDCWD, path, omit_atime, 0) == 0,
        "UTIME_OMIT failed"
    );
    let seen = attrs("/data/t")?;
    check!(
        seen.atime == 1 << 20 && seen.mtime == 3 << 20,
        "UTIME_OMIT gave {seen:?}"
    );
    let omit_both = Some([(1, UTIME_OMIT), (1, UTIME_OMIT)]);
    check!(
        as_user(BOB, || utimensat(AT_FDCWD, path, omit_both, 0)) == 0,
        "omitting both stamps"
    );
    check!(
        attrs("/data/t")? == seen,
        "omitting both stamps changed something"
    );

    // A stranger with write permission may touch, not backdate. The explicit
    // stamps above lie weeks after boot, so "now" (uptime) is below them.
    let before = vfs::now();
    check!(
        as_user(BOB, || utimensat(AT_FDCWD, path, None, 0)) == 0,
        "touch with write"
    );
    let seen = attrs("/data/t")?;
    let now = before..=vfs::now();
    check!(
        now.contains(&seen.atime) && now.contains(&seen.mtime),
        "touch gave {seen:?}"
    );
    let both_now = Some([(0, UTIME_NOW), (0, UTIME_NOW)]);
    check!(
        as_user(BOB, || utimensat(AT_FDCWD, path, both_now, 0)) == 0,
        "UTIME_NOW x2"
    );
    let half_now = Some([(0, UTIME_NOW), (0, UTIME_OMIT)]);
    check!(
        as_user(BOB, || utimensat(AT_FDCWD, path, half_now, 0)) == errno(EPERM),
        "a stranger touched only one stamp"
    );
    check!(
        as_user(BOB, || utimensat(AT_FDCWD, path, at_times(1, 1), 0)) == errno(EPERM),
        "a stranger set explicit times"
    );
    check!(chmod("/data/t", 0o644) == 0, "chmod failed");
    check!(
        as_user(BOB, || utimensat(AT_FDCWD, path, None, 0)) == errno(EACCES),
        "a stranger without write permission touched the file"
    );

    // Malformed arguments.
    let bad_nsec = Some([(0, 1_000_000_000), (0, 0)]);
    check!(
        utimensat(AT_FDCWD, path, bad_nsec, 0) == errno(EINVAL),
        "nsec of 1e9"
    );
    check!(
        utimensat(AT_FDCWD, path, None, 0x4) == errno(EINVAL),
        "unknown flag"
    );
    check!(
        utimensat(AT_FDCWD, Some("/data/none"), None, 0) == errno(ENOENT),
        "a missing file"
    );

    // ext2 holds 1970..2038: anything outside is clamped, as Linux clamps.
    check!(
        utimensat(AT_FDCWD, path, at_times(-5, 1 << 40), 0) == 0,
        "out-of-range times"
    );
    let seen = attrs("/data/t")?;
    check!(
        seen.atime == 0 && seen.mtime == i64::from(i32::MAX),
        "out-of-range times gave {seen:?}"
    );

    legacy_forms()?;
    data.check_clean()
}

/// `utime`, `utimes` and `futimesat` reach the same rules.
fn legacy_forms() -> Result<(), String> {
    let path = cstr("/data/t");
    let utimbuf: [i64; 2] = [5000, 6000];
    check!(
        syscall(
            SYS_UTIME,
            path.as_ptr() as u64,
            utimbuf.as_ptr() as u64,
            0,
            0
        ) == 0,
        "utime failed"
    );
    let seen = attrs("/data/t")?;
    check!(
        seen.atime == 5000 && seen.mtime == 6000,
        "utime gave {seen:?}"
    );
    check!(
        syscall(SYS_UTIME, path.as_ptr() as u64, 0, 0, 0) == 0,
        "utime(NULL) failed"
    );

    let timeval: [i64; 4] = [7000, 0, 8000, 999_999];
    check!(
        syscall(
            SYS_UTIMES,
            path.as_ptr() as u64,
            timeval.as_ptr() as u64,
            0,
            0
        ) == 0,
        "utimes failed"
    );
    let seen = attrs("/data/t")?;
    check!(
        seen.atime == 7000 && seen.mtime == 8000,
        "utimes gave {seen:?}"
    );
    let bad_usec: [i64; 4] = [1, 1_000_000, 1, 0];
    check!(
        syscall(
            SYS_UTIMES,
            path.as_ptr() as u64,
            bad_usec.as_ptr() as u64,
            0,
            0
        ) == errno(EINVAL),
        "utimes with a usec of 1e6"
    );

    let dir = open("/data", O_RDONLY);
    let name = cstr("t");
    let timeval: [i64; 4] = [9000, 0, 9500, 0];
    let ret = syscall(
        SYS_FUTIMESAT,
        dir,
        name.as_ptr() as u64,
        timeval.as_ptr() as u64,
        0,
    );
    close(dir);
    check!(ret == 0, "futimesat returned {ret:#x}");
    check!(attrs("/data/t")?.mtime == 9500, "futimesat did not land");
    Ok(())
}
