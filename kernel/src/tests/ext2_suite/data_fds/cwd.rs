//! The per-task working directory (issue #365): what `chdir`, `fchdir` and
//! `getcwd` accept and report, how `.` and `..` fold, the permission and error
//! rules, and what becomes of a directory that is removed under its owner.
//! What a relative path *means* once the directory is set is in `cwd_paths.rs`.

use crate::tests::hardening_suite::{in_space, Strict, SPACE};

use super::attrs::{as_user, chmod, ALICE};
use super::*;

pub(super) const SYS_STAT: u64 = 4;
pub(super) const SYS_GETCWD: u64 = 79;
pub(super) const SYS_CHDIR: u64 = 80;
pub(super) const SYS_FCHDIR: u64 = 81;
pub(super) const SYS_RMDIR: u64 = 84;
pub(super) const SYS_READLINK: u64 = 89;

pub(super) const ENOTDIR: u64 = 20;
pub(super) const ERANGE: u64 = 34;
pub(super) const ENAMETOOLONG: u64 = 36;

const O_DIRECTORY: u64 = 0o200000;

/// `chdir(path)`.
pub(super) fn chdir(path: &str) -> u64 {
    path_call(SYS_CHDIR, path, 0)
}

/// `fchdir(fd)`.
pub(super) fn fchdir(fd: u64) -> u64 {
    syscall(SYS_FCHDIR, fd, 0, 0, 0)
}

/// `mkdir(path, 0o755)`.
pub(super) fn mkdir(path: &str) -> u64 {
    path_call(SYS_MKDIR, path, 0o755)
}

/// `rmdir(path)`.
pub(super) fn rmdir(path: &str) -> u64 {
    path_call(SYS_RMDIR, path, 0)
}

/// `open(path, O_RDONLY | O_DIRECTORY)`.
pub(super) fn open_dir(path: &str) -> u64 {
    open(path, O_RDONLY | O_DIRECTORY)
}

/// The raw `getcwd(buf, size)` return with a 4 KiB kernel buffer.
fn getcwd_ret() -> u64 {
    let mut buf = [0u8; 4096];
    syscall(SYS_GETCWD, buf.as_mut_ptr() as u64, 4096, 0, 0)
}

/// The working directory as `getcwd` reports it. The raw syscall returns the
/// byte count including the NUL, which this checks against the string.
pub(super) fn getcwd() -> Result<String, String> {
    let mut buf = vec![0xAAu8; 4096];
    let ret = syscall(SYS_GETCWD, buf.as_mut_ptr() as u64, 4096, 0, 0);
    check!(ret <= 4096, "getcwd returned {ret:#x}");
    let text = &buf[..ret as usize];
    check!(text.last() == Some(&0), "getcwd is not NUL-terminated");
    let path = String::from_utf8(text[..text.len() - 1].to_vec()).map_err(|_| "not UTF-8")?;
    check!(
        text.len() == path.len() + 1,
        "getcwd stored an embedded NUL in {path:?}"
    );
    Ok(path)
}

/// The working directory is `want`.
pub(super) fn cwd_is(want: &str) -> Result<(), String> {
    let got = getcwd()?;
    check!(
        got == want,
        "the working directory is {got:?}, not {want:?}"
    );
    Ok(())
}

/// `chdir` then `getcwd`: what a directory change leads to.
fn go(path: &str) -> Result<String, String> {
    check!(chdir(path) == 0, "chdir({path:?}) failed");
    getcwd()
}

/// A fresh task is at the root; `chdir` moves it, absolute or relative, and
/// `getcwd` reports the real path with the byte count the raw syscall returns.
pub fn chdir_getcwd_roundtrip() -> Result<(), String> {
    let data = Data::new(0)?;
    cwd_is("/")?;
    check!(getcwd_ret() == 2, "the root is two bytes with its NUL");

    check!(mkdir("/data/a") == 0 && mkdir("/data/a/b") == 0, "mkdir");
    for dir in ["/data", "/tmp", "/", "/data/a/b", "/bin"] {
        check!(go(dir)? == dir, "chdir({dir:?}) did not land there");
    }
    check!(go("/data")? == "/data", "absolute");
    check!(go("a")? == "/data/a", "relative from /data");
    check!(go("b")? == "/data/a/b", "relative again");
    check!(getcwd_ret() == "/data/a/b".len() as u64 + 1, "byte count");
    check!(go("/")? == "/", "back to the root");
    // `/` is the one place a relative name and an absolute one differ by
    // nothing.
    check!(go("data")? == "/data", "relative from /");
    data.check_clean()
}

/// `.` and `..` fold lexically, repeated slashes collapse, and `..` never
/// climbs above the root.
pub fn dot_and_dotdot_fold() -> Result<(), String> {
    let data = Data::new(0)?;
    check!(mkdir("/data/a") == 0 && mkdir("/data/a/b") == 0, "mkdir");
    let cases = [
        ("/data/./a//b/../b/.", "/data/a/b"),
        ("../..", "/data"),
        ("a/b/../..", "/data"),
        ("./a/./b/", "/data/a/b"),
        ("/data/a/../../../data/a", "/data/a"),
        ("/../../..", "/"),
        ("..", "/"),
        ("../../data/a/b/../b", "/data/a/b"),
        ("/data/a/b/../../../../../tmp", "/tmp"),
        (".", "/tmp"),
    ];
    for (from, want) in cases {
        check!(go(from)? == want, "chdir({from:?}) should reach {want:?}");
    }
    data.check_clean()
}

/// Missing, not-a-directory and over-long targets fail as POSIX says, and a
/// failed `chdir` leaves the directory where it was.
pub fn chdir_errors_leave_the_cwd() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/f", b"file")?;
    put("/tmp/f", b"file")?;
    check!(chdir("/data") == 0, "chdir /data");
    let long = "a".repeat(5000);
    let just_fits = "a".repeat(4095);
    let sizeable = "a".repeat(200);
    let cases = [
        ("/data/missing", ENOENT),
        ("missing", ENOENT),
        ("missing/deeper", ENOENT),
        ("/data/f", ENOTDIR),
        ("f", ENOTDIR),
        ("/tmp/f", ENOTDIR),
        ("/data/f/x", ENOTDIR),
        (long.as_str(), ENAMETOOLONG),
        (just_fits.as_str(), ENAMETOOLONG),
        (sizeable.as_str(), ENOENT),
    ];
    for (path, want) in cases {
        let shown = &path[..path.len().min(24)];
        let got = chdir(path);
        check!(
            got == errno(want),
            "chdir({shown:?}..) -> {got:#x}, want -{want}"
        );
        check!(getcwd()? == "/data", "a failed chdir({shown:?}..) moved");
    }
    data.check_clean()
}

/// `chdir` needs search (execute) permission on the target and on every
/// directory leading to it; root needs none. A directory that later loses the
/// bit stops resolving relative names for its occupant, who still knows where
/// they are.
pub fn chdir_needs_search_permission() -> Result<(), String> {
    let data = Data::new(0)?;
    let modes = [
        ("/data/locked", 0o700),
        ("/data/nosearch", 0o644),
        ("/data/xonly", 0o711),
        ("/data/pub", 0o755),
    ];
    for (path, mode) in modes {
        check!(path_call(SYS_MKDIR, path, mode) == 0, "mkdir {path}");
    }
    check!(mkdir("/data/locked/inner") == 0, "mkdir inner");
    check!(chdir("/data/locked/inner") == 0, "root is not stopped");
    cwd_is("/data/locked/inner")?;
    check!(chdir("/") == 0, "back to the root");

    let denied = as_user(ALICE, || {
        ["/data/locked", "/data/locked/inner", "/data/nosearch"].map(chdir)
    });
    check!(
        denied == [errno(EACCES); 3],
        "a stranger entered a directory it cannot search: {denied:x?}"
    );
    cwd_is("/")?;

    check!(
        as_user(ALICE, || chdir("/data/xonly")) == 0,
        "a directory with only the search bit"
    );
    check!(
        as_user(ALICE, || chdir("/data/pub")) == 0,
        "a world-searchable directory"
    );
    cwd_is("/data/pub")?;

    // Take the bit away from under the occupant.
    check!(chmod("/data/pub", 0o700) == 0, "chmod");
    let after = as_user(ALICE, || {
        [
            open("f", O_CREAT | O_WRONLY),
            chdir("."),
            chdir("/data/pub"),
        ]
    });
    check!(
        after == [errno(EACCES); 3],
        "relative names in a directory that lost its search bit: {after:x?}"
    );
    cwd_is("/data/pub")?;
    data.check_clean()
}

/// `fchdir` follows a directory descriptor, and refuses everything else.
pub fn fchdir_directory_descriptors() -> Result<(), String> {
    let data = Data::new(0)?;
    check!(mkdir("/data/d") == 0, "mkdir");
    put("/data/file", b"x")?;
    put("/tmp/file", b"x")?;

    for dir in ["/data/d", "/tmp", "/"] {
        let fd = open_dir(dir);
        check!(fd < 16, "open({dir}) returned {fd:#x}");
        check!(fchdir(fd) == 0, "fchdir({dir}) failed");
        cwd_is(dir)?;
        check!(close(fd) == 0, "close");
    }
    // The directory outlives the descriptor.
    cwd_is("/")?;

    let mut pipe = [0i32; 2];
    check!(
        syscall(SYS_PIPE, pipe.as_mut_ptr() as u64, 0, 0, 0) == 0,
        "pipe"
    );
    let not_dirs = [
        open("/data/file", O_RDONLY),
        open("/tmp/file", O_RDONLY),
        open("/dev/null", O_RDONLY),
        pipe[0] as u64,
    ];
    check!(chdir("/data/d") == 0, "chdir");
    for fd in not_dirs {
        check!(
            fchdir(fd) == errno(ENOTDIR),
            "fchdir({fd}) on a non-directory"
        );
        close(fd);
    }
    close(pipe[1] as u64);
    for fd in [7u64, 9999, u64::MAX] {
        check!(fchdir(fd) == errno(EBADF), "fchdir({fd:#x}) on a closed fd");
    }
    cwd_is("/data/d")?;

    // A descriptor opened by root does not lend its owner's rights.
    check!(path_call(SYS_MKDIR, "/data/locked", 0o700) == 0, "mkdir");
    let held = open_dir("/data/locked");
    check!(held < 16, "root opens /data/locked");
    check!(
        as_user(ALICE, || fchdir(held)) == errno(EACCES),
        "a stranger entered through root's descriptor"
    );
    cwd_is("/data/d")?;
    // A directory removed since it was opened cannot be entered. (Only `/tmp`
    // can lose directories: ext2 has no `rmdir` yet.)
    check!(mkdir("/tmp/vanishing") == 0, "mkdir");
    let gone = open_dir("/tmp/vanishing");
    check!(chdir("/") == 0, "chdir /");
    let removed = rmdir("/tmp/vanishing");
    check!(removed == 0, "rmdir returned {removed:#x}");
    check!(
        fchdir(gone) == errno(ENOENT),
        "fchdir of a removed directory"
    );
    close(gone);
    close(held);
    data.check_clean()
}

/// `getcwd` needs room for the path and its NUL (`ERANGE` otherwise, writing
/// nothing), stores exactly that many bytes, and refuses a buffer that is not
/// user memory. Unreadable path strings are `EFAULT` for every syscall.
pub fn getcwd_bounds_and_hostile_pointers() -> Result<(), String> {
    let data = Data::new(0)?;
    check!(chdir("/data") == 0, "chdir");
    // "/data" and its NUL are six bytes.
    for (size, want) in [(6u64, 6u64), (7, 6), (4096, 6), (u64::MAX, 6)] {
        let mut buf = [0xAAu8; 16];
        let got = syscall(SYS_GETCWD, buf.as_mut_ptr() as u64, size, 0, 0);
        check!(got == want, "getcwd(size {size:#x}) -> {got:#x}");
        check!(&buf[..6] == b"/data\0", "getcwd wrote {:?}", &buf[..6]);
        check!(buf[6..].iter().all(|b| *b == 0xAA), "getcwd wrote past NUL");
    }
    for size in [0u64, 1, 5] {
        let mut buf = [0xAAu8; 16];
        let got = syscall(SYS_GETCWD, buf.as_mut_ptr() as u64, size, 0, 0);
        check!(got == errno(ERANGE), "getcwd(size {size}) -> {got:#x}");
        check!(buf.iter().all(|b| *b == 0xAA), "ERANGE still wrote");
    }

    in_space(|| -> Result<(), String> {
        let _strict = Strict::on();
        let good = syscall(SYS_GETCWD, SPACE, 64, 0, 0);
        check!(good == 6, "a mapped buffer -> {good:#x}");
        for bad in [0xdead_0000u64, 0, u64::MAX - 3] {
            let got = syscall(SYS_GETCWD, bad, 64, 0, 0);
            check!(got == errno(EFAULT), "getcwd into {bad:#x} -> {got:#x}");
            for nr in [SYS_CHDIR, SYS_MKDIR, SYS_RMDIR, SYS_UNLINK, SYS_TRUNCATE] {
                let got = syscall(nr, bad, 0, 0, 0);
                check!(got == errno(EFAULT), "syscall {nr} on {bad:#x} -> {got:#x}");
            }
            let got = syscall(SYS_READLINK, bad, SPACE, 8, 0);
            check!(got == errno(EFAULT), "readlink on {bad:#x} -> {got:#x}");
            let got = syscall(SYS_RENAME, bad, bad, 0, 0);
            check!(got == errno(EFAULT), "rename on {bad:#x} -> {got:#x}");
            let got = syscall(SYS_OPENAT, AT_FDCWD, bad, O_RDONLY, 0);
            check!(got == errno(EFAULT), "openat on {bad:#x} -> {got:#x}");
        }
        Ok(())
    })?;
    cwd_is("/data")?;
    data.check_clean()
}

/// Removing somebody's working directory is allowed (nothing pins it, as on
/// Linux); the occupant then finds nothing to resolve names against. Only
/// `/tmp` is exercised: ext2 does not implement `rmdir` yet, so `/data`
/// directories cannot go away.
pub fn removed_cwd() -> Result<(), String> {
    let data = Data::new(0)?;
    let gone = "/tmp/gone";
    check!(mkdir(gone) == 0 && chdir(gone) == 0, "set up {gone}");
    // `rmdir(".")` is `EINVAL` (not a removal under another name), and the
    // parent still holds this directory.
    check!(rmdir(".") == errno(EINVAL), "rmdir(.)");
    check!(rmdir("./") == errno(EINVAL), "rmdir(./)");
    check!(rmdir("..") != 0, "rmdir(..) removed the parent");
    let removed = rmdir(gone);
    check!(removed == 0, "rmdir of the working directory: {removed:#x}");

    check!(
        getcwd_ret() == errno(ENOENT),
        "getcwd of a removed directory"
    );
    for (what, got) in [
        ("create", open("x", O_CREAT | O_WRONLY)),
        ("open .", open(".", O_RDONLY)),
        ("mkdir", mkdir("sub")),
        ("stat", stat_call(".")),
    ] {
        check!(got == errno(ENOENT), "{what} in a removed cwd -> {got:#x}");
    }
    // Leaving works, and the name can be used again.
    check!(go("/tmp")? == "/tmp", "leaving works");
    check!(mkdir(gone) == 0, "mkdir again");
    data.check_clean()
}

/// `stat(path)`'s raw return.
fn stat_call(path: &str) -> u64 {
    let mut stat = [0u8; 144];
    syscall(
        SYS_STAT,
        cstr(path).as_ptr() as u64,
        stat.as_mut_ptr() as u64,
        0,
        0,
    )
}

/// `/proc/self/cwd` reads back the working directory, truncated like any
/// `readlink`.
pub fn proc_self_cwd_reports_the_cwd() -> Result<(), String> {
    let data = Data::new(0)?;
    let link = |path: &str, size: u64| {
        let mut buf = [0u8; 64];
        let got = syscall(
            SYS_READLINK,
            cstr(path).as_ptr() as u64,
            buf.as_mut_ptr() as u64,
            size,
            0,
        );
        (got, buf)
    };
    check!(mkdir("/data/a") == 0, "mkdir");
    check!(link("/proc/self/cwd", 64).0 == 1, "the root is one byte");
    check!(chdir("/data/a") == 0, "chdir");
    let (got, buf) = link("/proc/self/cwd", 64);
    check!(got == 7 && &buf[..7] == b"/data/a", "readlink -> {got:#x}");
    let (got, buf) = link("/proc/self/cwd", 3);
    check!(
        got == 3 && &buf[..3] == b"/da",
        "a short buffer -> {got:#x}"
    );
    check!(
        link("/proc/self/cwd", 0).0 == errno(EINVAL),
        "a zero buffer"
    );
    // The link is a path like any other, so it resolves from the cwd too.
    check!(
        link("../../proc/self/cwd", 64).0 == 7,
        "a relative path to the link"
    );
    check!(
        link("cwd", 64).0 == errno(ENOENT),
        "there is no ./cwd link in a plain directory"
    );
    data.check_clean()
}
