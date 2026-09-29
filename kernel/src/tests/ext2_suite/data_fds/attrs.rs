//! `chmod`, `chown` and `utimensat` (and their variants) on `/data` (issue
//! #345): the owner / non-owner / root matrix, setuid and setgid clearing,
//! `UTIME_NOW`/`UTIME_OMIT`, the descriptor forms, `EROFS`, and durability
//! across a remount, all through the syscall surface.

use super::*;

const SYS_STAT: u64 = 4;
const SYS_CHMOD: u64 = 90;
pub(super) const SYS_FCHMOD: u64 = 91;
const SYS_CHOWN: u64 = 92;
const SYS_FCHOWN: u64 = 93;
const SYS_LCHOWN: u64 = 94;
const SYS_UTIME: u64 = 132;
const SYS_UTIMES: u64 = 235;
const SYS_FCHOWNAT: u64 = 260;
const SYS_FUTIMESAT: u64 = 261;
const SYS_FCHMODAT: u64 = 268;
const SYS_UTIMENSAT: u64 = 280;

const EPERM: u64 = 1;
const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const AT_EMPTY_PATH: u64 = 0x1000;
const UTIME_NOW: i64 = (1 << 30) - 1;
const UTIME_OMIT: i64 = (1 << 30) - 2;
/// `chown`'s "leave this id alone".
const KEEP: u64 = u32::MAX as u64;

/// The users of the matrix: `ALICE` owns the files under test, `BOB` is a
/// stranger in another group.
const ALICE: Cred = Cred::new(1000, 100, 0, 0, 0);
const BOB: Cred = Cred::new(2000, 200, 0, 0, 0);

/// What `stat` reports about a node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Attrs {
    pub(super) mode: u32,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) atime: i64,
    pub(super) mtime: i64,
}

fn word(stat: &[u8; 144], offset: usize) -> i64 {
    i64::from_le_bytes(stat[offset..offset + 8].try_into().unwrap())
}

fn half(stat: &[u8; 144], offset: usize) -> u32 {
    u32::from_le_bytes(stat[offset..offset + 4].try_into().unwrap())
}

fn decode(stat: &[u8; 144]) -> Attrs {
    Attrs {
        mode: half(stat, 24) & 0o7777,
        uid: half(stat, 28),
        gid: half(stat, 32),
        atime: word(stat, 72),
        mtime: word(stat, 88),
    }
}

/// `stat(path)` decoded.
pub(super) fn attrs(path: &str) -> Result<Attrs, String> {
    let mut stat = [0u8; 144];
    let ret = syscall(
        SYS_STAT,
        cstr(path).as_ptr() as u64,
        stat.as_mut_ptr() as u64,
        0,
        0,
    );
    check!(ret == 0, "stat({path}) returned {ret:#x}");
    Ok(decode(&stat))
}

/// `fstat(fd)` decoded.
fn fattrs(fd: u64) -> Result<Attrs, String> {
    let mut stat = [0u8; 144];
    let ret = syscall(SYS_FSTAT, fd, stat.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "fstat({fd}) returned {ret:#x}");
    Ok(decode(&stat))
}

pub(super) fn chmod(path: &str, mode: u64) -> u64 {
    path_call(SYS_CHMOD, path, mode)
}

pub(super) fn chown(path: &str, uid: u64, gid: u64) -> u64 {
    syscall(SYS_CHOWN, cstr(path).as_ptr() as u64, uid, gid, 0)
}

fn fchownat(dirfd: u64, path: &str, uid: u64, gid: u64, flags: u64) -> u64 {
    let path = cstr(path);
    process::linux::dispatch_args5_for_test(
        SYS_FCHOWNAT,
        dirfd,
        path.as_ptr() as u64,
        uid,
        gid,
        flags,
    )
}

/// `utimensat(dirfd, path, times, flags)`; `None` times is `NULL`, `None`
/// path is `NULL` (the `futimens` form).
pub(super) fn utimensat(
    dirfd: u64,
    path: Option<&str>,
    times: Option<[(i64, i64); 2]>,
    flags: u64,
) -> u64 {
    let path = path.map(cstr);
    let path_ptr = path.as_ref().map_or(0, |path| path.as_ptr() as u64);
    let words = times.map(|[(a_sec, a_nsec), (m_sec, m_nsec)]| [a_sec, a_nsec, m_sec, m_nsec]);
    let times_ptr = words.as_ref().map_or(0, |words| words.as_ptr() as u64);
    syscall(SYS_UTIMENSAT, dirfd, path_ptr, times_ptr, flags)
}

/// Explicit whole-second times for `utimensat`.
pub(super) fn at_times(atime: i64, mtime: i64) -> Option<[(i64, i64); 2]> {
    Some([(atime, 0), (mtime, 0)])
}

/// Run `body` with the task's credentials set to `cred`, restoring root after.
fn as_user<T>(cred: Cred, body: impl FnOnce() -> T) -> T {
    credentials::set(task::current(), cred);
    let result = body();
    credentials::set(task::current(), Cred::ROOT);
    result
}

/// A root-created file handed to `ALICE` with `mode`.
fn alice_file(path: &str, mode: u64) -> Result<(), String> {
    put(path, b"attr")?;
    check!(chown(path, 1000, 100) == 0, "root could not chown {path}");
    check!(chmod(path, mode) == 0, "root could not chmod {path}");
    Ok(())
}

/// `chmod`: the owner and root may, a stranger may not (`EPERM`, not
/// `EACCES`); search permission still applies to the path; setgid is dropped
/// for an owner outside the file's group.
pub fn chmod_matrix() -> Result<(), String> {
    let data = Data::new(0)?;
    alice_file("/data/f", 0o644)?;

    check!(
        as_user(ALICE, || chmod("/data/f", 0o600)) == 0,
        "the owner's chmod failed"
    );
    check!(
        attrs("/data/f")?.mode == 0o600,
        "the owner's chmod did not land"
    );
    check!(
        as_user(BOB, || chmod("/data/f", 0o777)) == errno(EPERM),
        "a stranger's chmod was not EPERM"
    );
    check!(
        attrs("/data/f")?.mode == 0o600,
        "a refused chmod changed the mode"
    );
    check!(chmod("/data/f", 0o4751) == 0, "root's chmod failed");
    check!(
        attrs("/data/f")?.mode == 0o4751,
        "root's chmod did not land"
    );

    // Setgid survives for a member of the file's group, not for an outsider.
    check!(
        as_user(ALICE, || chmod("/data/f", 0o2755)) == 0,
        "chmod g+s failed"
    );
    check!(attrs("/data/f")?.mode == 0o2755, "a member lost setgid");
    check!(chown("/data/f", KEEP, 300) == 0, "root chgrp failed");
    check!(
        as_user(ALICE, || chmod("/data/f", 0o2755)) == 0,
        "chmod g+s failed"
    );
    check!(attrs("/data/f")?.mode == 0o755, "an outsider set setgid");

    // A path the caller cannot search is EACCES before any ownership rule.
    check!(
        path_call(SYS_MKDIR, "/data/locked", 0o700) == 0,
        "mkdir failed"
    );
    alice_file("/data/locked/x", 0o644)?;
    check!(
        as_user(ALICE, || chmod("/data/locked/x", 0o600)) == errno(EACCES),
        "chmod through an unsearchable directory"
    );
    check!(
        chmod("/data/missing", 0o600) == errno(ENOENT),
        "chmod of a missing file"
    );
    check!(chmod("", 0o600) == errno(ENOENT), "chmod of an empty path");

    // fchmodat resolves relative to a directory descriptor.
    let dir = open("/data", O_RDONLY);
    check!(dir < 16, "open(/data) returned {dir:#x}");
    check!(
        syscall(SYS_FCHMODAT, dir, cstr("f").as_ptr() as u64, 0o640, 0) == 0,
        "fchmodat failed"
    );
    close(dir);
    check!(attrs("/data/f")?.mode == 0o640, "fchmodat did not land");
    data.check_clean()
}

/// `fchmod`/`fchown`/`futimens` act on the open file, check ownership at call
/// time, and reject what is not a file.
pub fn descriptor_forms() -> Result<(), String> {
    let data = Data::new(0)?;
    alice_file("/data/f", 0o644)?;
    let fd = open("/data/f", O_RDONLY);
    check!(fd < 16, "open returned {fd:#x}");

    check!(
        as_user(ALICE, || syscall(SYS_FCHMOD, fd, 0o604, 0, 0)) == 0,
        "fchmod failed"
    );
    check!(fattrs(fd)?.mode == 0o604, "fstat does not see the fchmod");
    check!(
        as_user(BOB, || syscall(SYS_FCHMOD, fd, 0o777, 0, 0)) == errno(EPERM),
        "a stranger's fchmod through a shared fd was allowed"
    );
    check!(
        syscall(SYS_FCHOWN, fd, 1000, 300, 0) == 0,
        "root fchown failed"
    );
    check!(fattrs(fd)?.gid == 300, "fchown did not land");
    check!(
        utimensat(fd, None, at_times(10, 20), 0) == 0,
        "futimens failed"
    );
    let seen = fattrs(fd)?;
    check!(
        seen.atime == 10 && seen.mtime == 20,
        "futimens gave {seen:?}"
    );
    check!(
        fchownat(fd, "", 1000, 100, AT_EMPTY_PATH) == 0,
        "fchownat(AT_EMPTY_PATH) failed"
    );
    check!(
        attrs("/data/f")?.gid == 100,
        "fchownat(AT_EMPTY_PATH) did not land"
    );

    // A snapshot descriptor (the /data directory itself) works the same way.
    let dir = open("/data", O_RDONLY);
    check!(
        syscall(SYS_FCHMOD, dir, 0o711, 0, 0) == 0,
        "fchmod of a directory fd"
    );
    check!(
        attrs("/data")?.mode == 0o711,
        "directory fchmod did not land"
    );
    check!(
        fattrs(dir)?.mode == 0o711,
        "fstat of the directory fd is stale"
    );
    close(dir);

    // Bad descriptors.
    check!(
        syscall(SYS_FCHMOD, 99, 0o600, 0, 0) == errno(EBADF),
        "fchmod(99)"
    );
    check!(
        syscall(SYS_FCHMOD, 12, 0o600, 0, 0) == errno(EBADF),
        "fchmod(closed fd)"
    );
    let mut pipe = [0i32; 2];
    check!(
        syscall(SYS_PIPE, pipe.as_mut_ptr() as u64, 0, 0, 0) == 0,
        "pipe failed"
    );
    check!(
        syscall(SYS_FCHOWN, pipe[1] as u64, 0, 0, 0) == errno(EINVAL),
        "fchown(pipe)"
    );
    check!(
        utimensat(pipe[0] as u64, None, None, 0) == errno(EINVAL),
        "futimens on a pipe"
    );
    check!(
        utimensat(AT_FDCWD, None, None, 0) == errno(EFAULT),
        "utimensat with no path and no fd"
    );
    close(pipe[0] as u64);
    close(pipe[1] as u64);
    close(fd);
    data.check_clean()
}

/// `chown`: only root gives a file away; the owner may pick their own group;
/// a regular file loses setuid/setgid whoever changes its owner, a directory
/// keeps its setgid.
pub fn chown_rules() -> Result<(), String> {
    let data = Data::new(0)?;
    alice_file("/data/prog", 0o6755)?;
    check!(chown("/data/prog", KEEP, 300) == 0, "root chgrp failed");
    check!(chmod("/data/prog", 0o6755) == 0, "root chmod failed");

    check!(
        as_user(ALICE, || chown("/data/prog", 2000, KEEP)) == errno(EPERM),
        "the owner gave the file away"
    );
    check!(
        as_user(ALICE, || chown("/data/prog", KEEP, 200)) == errno(EPERM),
        "the owner moved the file to a group they are not in"
    );
    check!(
        as_user(BOB, || chown("/data/prog", 2000, 200)) == errno(EPERM),
        "a stranger took the file"
    );
    check!(
        attrs("/data/prog")?.mode == 0o6755,
        "a refused chown cleared bits"
    );
    check!(
        as_user(ALICE, || chown("/data/prog", 1000, 100)) == 0,
        "the owner could not move the file to their own group"
    );
    let after = attrs("/data/prog")?;
    check!(
        after.uid == 1000 && after.gid == 100 && after.mode == 0o755,
        "the owner's chown gave {after:?}"
    );
    check!(chmod("/data/prog", 0o6755) == 0, "root chmod failed");
    check!(
        syscall(SYS_LCHOWN, cstr("/data/prog").as_ptr() as u64, 0, 0, 0) == 0,
        "lchown"
    );
    let after = attrs("/data/prog")?;
    check!(
        after.uid == 0 && after.gid == 0 && after.mode == 0o755,
        "root's chown gave {after:?} (setuid must go for root too)"
    );
    check!(
        as_user(BOB, || chown("/data/prog", KEEP, KEEP)) == 0,
        "chown(-1, -1)"
    );
    check!(
        attrs("/data/prog")? == after,
        "chown(-1, -1) changed something"
    );

    check!(
        path_call(SYS_MKDIR, "/data/shared", 0o755) == 0,
        "mkdir failed"
    );
    check!(chmod("/data/shared", 0o2775) == 0, "chmod of the directory");
    check!(
        chown("/data/shared", 1000, 100) == 0,
        "chown of the directory"
    );
    check!(
        attrs("/data/shared")?.mode == 0o2775,
        "a directory lost setgid"
    );

    // fchownat flags, and ids ext2 cannot hold.
    check!(
        fchownat(AT_FDCWD, "/data/prog", 5, 6, AT_SYMLINK_NOFOLLOW) == 0,
        "fchownat(AT_SYMLINK_NOFOLLOW)"
    );
    check!(attrs("/data/prog")?.uid == 5, "fchownat did not land");
    check!(
        fchownat(AT_FDCWD, "/data/prog", 5, 6, 0x4) == errno(EINVAL),
        "unknown flag"
    );
    check!(
        fchownat(AT_FDCWD, "", 5, 6, 0) == errno(ENOENT),
        "empty path"
    );
    check!(
        chown("/data/prog", 70_000, KEEP) == errno(EINVAL),
        "a 17-bit uid on ext2"
    );
    check!(
        attrs("/data/prog")?.uid == 5,
        "a refused uid changed the owner"
    );
    data.check_clean()
}

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

/// Every change is refused with `EROFS` on a read-only device and leaves the
/// attributes as they were.
pub fn read_only_attrs() -> Result<(), String> {
    let data = Data::new(0)?;
    alice_file("/data/ro", 0o644)?;
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync failed");
    data.disk.set_read_only(true);
    data.remount()?;
    let before = attrs("/data/ro")?;

    check!(
        chmod("/data/ro", 0o600) == errno(EROFS),
        "chmod on a read-only volume"
    );
    check!(
        chown("/data/ro", 0, 0) == errno(EROFS),
        "chown on a read-only volume"
    );
    check!(
        utimensat(AT_FDCWD, Some("/data/ro"), None, 0) == errno(EROFS),
        "utimensat on a read-only volume"
    );
    let fd = open("/data/ro", O_RDONLY);
    check!(
        syscall(SYS_FCHMOD, fd, 0o600, 0, 0) == errno(EROFS),
        "fchmod, read-only"
    );
    close(fd);
    check!(
        attrs("/data/ro")? == before,
        "a refused change altered the attributes"
    );

    data.disk.set_read_only(false);
    data.remount()?;
    data.check_clean()
}

/// Mode, owner and times reach the disk: the change dirties the volume first,
/// `sync` marks it clean after, and a remount reads back exactly what was set.
pub fn attrs_survive_remount() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/keep", b"kept")?;
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync failed");
    check!(
        raw_state(data.disk) & 1 == 1,
        "the volume is not clean after sync"
    );

    check!(chmod("/data/keep", 0o4640) == 0, "chmod failed");
    check!(
        raw_state(data.disk) & 1 == 0,
        "chmod reached the disk behind a clean flag"
    );
    check!(chown("/data/keep", 1234, 567) == 0, "chown failed");
    check!(
        utimensat(AT_FDCWD, Some("/data/keep"), at_times(1111, 2222), 0) == 0,
        "utimensat failed"
    );
    let set = attrs("/data/keep")?;
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync failed");
    check!(
        raw_state(data.disk) & 1 == 1,
        "sync did not mark the volume clean"
    );

    data.remount()?;
    let back = attrs("/data/keep")?;
    let want = Attrs {
        mode: 0o640, // chown cleared the setuid bit
        uid: 1234,
        gid: 567,
        atime: 1111,
        mtime: 2222,
    };
    check!(set == want, "before the remount: {set:?}");
    check!(back == want, "after the remount: {back:?}");
    check!(slurp("/data/keep")? == b"kept", "the contents changed");
    data.check_clean()
}
