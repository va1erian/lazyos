//! `statx` (issue #348): the fields it fills from the stat path, mask and flag
//! handling, `AT_EMPTY_PATH`, directory-relative lookups, and its refusals.

use crate::tests::hardening_suite::{in_space, Strict, SPACE};

use super::inspect::{O_DIRECTORY, S_IFDIR, S_IFREG};
use super::*;

const SYS_STAT: u64 = 4;
const SYS_LSTAT: u64 = 6;
const SYS_NEWFSTATAT: u64 = 262;
const SYS_STATX: u64 = 332;

const ENAMETOOLONG: u64 = 36;

const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const AT_NO_AUTOMOUNT: u64 = 0x800;
const AT_EMPTY_PATH: u64 = 0x1000;
const AT_STATX_FORCE_SYNC: u64 = 0x2000;
const AT_STATX_DONT_SYNC: u64 = 0x4000;

/// The fields `statx` claims for every file: no timestamps.
const STATX_CLAIMED: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x100 | 0x200 | 0x400;
/// `STATX_ATIME | STATX_BTIME | STATX_CTIME | STATX_MTIME`.
const STATX_TIMES: u32 = 0x20 | 0x800 | 0x80 | 0x40;
const STATX_RESERVED: u64 = 0x8000_0000;

/// A decoded `struct statx`.
struct Statx([u8; 256]);

impl Statx {
    fn u16(&self, at: usize) -> u16 {
        u16::from_le_bytes(self.0[at..at + 2].try_into().unwrap())
    }

    fn u32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.0[at..at + 4].try_into().unwrap())
    }

    fn u64(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.0[at..at + 8].try_into().unwrap())
    }

    fn mask(&self) -> u32 {
        self.u32(0)
    }
    fn nlink(&self) -> u32 {
        self.u32(16)
    }
    fn uid(&self) -> u32 {
        self.u32(20)
    }
    fn gid(&self) -> u32 {
        self.u32(24)
    }
    fn mode(&self) -> u32 {
        u32::from(self.u16(28))
    }
    fn ino(&self) -> u64 {
        self.u64(32)
    }
    fn size(&self) -> u64 {
        self.u64(40)
    }
    fn blocks(&self) -> u64 {
        self.u64(48)
    }
}

/// `statx(dirfd, path, flags, mask)`: the decoded reply, or the raw error.
fn statx(dirfd: u64, path: &[u8], flags: u64, mask: u64) -> Result<Statx, u64> {
    let mut buf = [0xEEu8; 256];
    let ret = process::linux::dispatch_args5_for_test(
        SYS_STATX,
        dirfd,
        path.as_ptr() as u64,
        flags,
        mask,
        buf.as_mut_ptr() as u64,
    );
    if ret == 0 {
        Ok(Statx(buf))
    } else {
        Err(ret)
    }
}

/// `statx` of an absolute path with the usual "everything" mask.
fn statx_path(path: &str) -> Result<Statx, u64> {
    statx(AT_FDCWD, &cstr(path), 0, 0x7ff)
}

/// `stat`'s `st_ino` for `path`.
fn stat_ino(path: &str) -> Result<u64, String> {
    let mut stat = [0u8; 144];
    let ret = syscall(
        SYS_STAT,
        cstr(path).as_ptr() as u64,
        stat.as_mut_ptr() as u64,
        0,
        0,
    );
    check!(ret == 0, "stat({path}) returned {ret:#x}");
    Ok(u64::from_le_bytes(stat[8..16].try_into().unwrap()))
}

/// A regular file and a directory report what `stat` does, plus the fields
/// `stat` has no room for, and claim no timestamps (birth time included).
pub fn statx_reports_stat_fields() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/s", b"hello world")?;
    let file = statx_path("/data/s").map_err(|code| format!("statx returned {code:#x}"))?;
    check!(file.mask() == STATX_CLAIMED, "mask {:#x}", file.mask());
    check!(file.mask() & STATX_TIMES == 0, "a timestamp was claimed");
    check!(file.mode() == S_IFREG | 0o644, "mode {:#o}", file.mode());
    check!(file.size() == 11, "size {}", file.size());
    check!(file.blocks() == 1, "blocks {}", file.blocks());
    check!(file.nlink() == 1, "nlink {}", file.nlink());
    check!(
        file.uid() == 0 && file.gid() == 0,
        "owner {}:{}",
        file.uid(),
        file.gid()
    );
    check!(file.u32(4) == 4096, "blksize {}", file.u32(4));
    check!(file.ino() == stat_ino("/data/s")?, "ino differs from stat");
    // Reserved and unclaimed space is zero: attributes, the four timestamps,
    // device numbers and the alignment fields.
    check!(
        file.0[8..16].iter().all(|b| *b == 0),
        "attributes are not zero"
    );
    check!(
        file.0[56..256].iter().all(|b| *b == 0),
        "the tail is not zeroed"
    );

    let dir = statx_path("/data").map_err(|code| format!("statx dir {code:#x}"))?;
    check!(
        dir.mode() & 0o170000 == S_IFDIR,
        "mode of /data {:#o}",
        dir.mode()
    );
    let root = statx(AT_FDCWD, b"\0", AT_EMPTY_PATH, 0x7ff);
    check!(
        root.map(|s| s.mode() & 0o170000) == Ok(S_IFDIR),
        "AT_EMPTY_PATH with AT_FDCWD is not the root directory"
    );
    check!(path_call(SYS_UNLINK, "/data/s", 0) == 0, "unlink failed");
    data.check_clean()
}

/// `(st_uid, st_gid)` of a `struct stat`.
fn stat_owner(stat: &[u8; 144]) -> (u32, u32) {
    (
        u32::from_le_bytes(stat[28..32].try_into().unwrap()),
        u32::from_le_bytes(stat[32..36].try_into().unwrap()),
    )
}

/// `(return value, (st_uid, st_gid))` of a path-taking stat call.
fn stat_call(nr: u64, path: &str) -> (u64, (u32, u32)) {
    let mut stat = [0u8; 144];
    let path = cstr(path);
    let (name, out) = (path.as_ptr() as u64, stat.as_mut_ptr() as u64);
    let ret = if nr == SYS_NEWFSTATAT {
        process::linux::dispatch_args5_for_test(nr, AT_FDCWD, name, out, 0, 0)
    } else {
        syscall(nr, name, out, 0, 0)
    };
    (ret, stat_owner(&stat))
}

/// Every stat call on `path` (and, once opened, on descriptor `fd`) must say
/// the file belongs to `uid:gid`: `stat`, `lstat`, `newfstatat`, `fstat` and
/// `statx`, by path and by descriptor.
fn expect_owner(path: &str, fd: u64, uid: u32, gid: u32) -> Result<(), String> {
    let want = (uid, gid);
    for (nr, name) in [
        (SYS_STAT, "stat"),
        (SYS_LSTAT, "lstat"),
        (SYS_NEWFSTATAT, "newfstatat"),
    ] {
        let (ret, owner) = stat_call(nr, path);
        check!(ret == 0, "{name}({path}) returned {ret:#x}");
        check!(
            owner == want,
            "{name}({path}) owner {owner:?}, want {want:?}"
        );
    }
    let mut stat = [0u8; 144];
    check!(
        syscall(SYS_FSTAT, fd, stat.as_mut_ptr() as u64, 0, 0) == 0,
        "fstat({path}) failed"
    );
    check!(
        stat_owner(&stat) == want,
        "fstat({path}) owner {:?}",
        stat_owner(&stat)
    );
    for (label, reply) in [
        ("by path", statx_path(path)),
        ("by fd", statx(fd, b"\0", AT_EMPTY_PATH, 0x7ff)),
    ] {
        let reply = reply.map_err(|code| format!("statx {path} {label}: {code:#x}"))?;
        check!(
            (reply.uid(), reply.gid()) == want,
            "statx {path} {label}: owner {}:{}",
            reply.uid(),
            reply.gid()
        );
    }
    Ok(())
}

/// The owner recorded at creation is what the whole stat family reports, on
/// both kinds of descriptor: VFS-backed (`/data`) and snapshot (`/tmp`), for
/// files and directories.
pub fn stat_family_reports_the_owner() -> Result<(), String> {
    let data = Data::new(0)?;
    let umask = crate::fs::abi_set_umask(0);
    for dir in ["/data/pub", "/tmp/pub"] {
        check!(path_call(SYS_MKDIR, dir, 0o777) == 0, "mkdir {dir} failed");
    }
    crate::fs::abi_set_umask(umask);
    credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
    put("/data/pub/mine", b"m")?;
    put("/tmp/pub/mine", b"m")?;
    check!(
        path_call(SYS_MKDIR, "/data/pub/sub", 0o755) == 0,
        "mkdir sub"
    );
    credentials::set(task::current(), Cred::ROOT);
    put("/data/pub/root", b"r")?;

    for path in ["/data/pub/mine", "/tmp/pub/mine", "/data/pub/sub"] {
        let fd = open(path, O_RDONLY);
        check!(fd < 16, "open({path}) returned {fd:#x}");
        expect_owner(path, fd, 1000, 100)?;
        close(fd);
    }
    // A root-owned file next to them still says root.
    let fd = open("/data/pub/root", O_RDONLY);
    expect_owner("/data/pub/root", fd, 0, 0)?;
    close(fd);
    for path in ["/data/pub/mine", "/data/pub/root", "/tmp/pub/mine"] {
        check!(path_call(SYS_UNLINK, path, 0) == 0, "unlink {path} failed");
    }
    data.check_clean()
}

/// `AT_EMPTY_PATH` names the descriptor (files, snapshots and directories),
/// and a relative path is looked up in the directory descriptor.
pub fn statx_empty_path_and_dirfd() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/s", b"hello world")?;
    put("/tmp/t", b"abc")?;
    let file = open("/data/s", O_RDONLY);
    let snapshot = open("/tmp/t", O_RDONLY);
    let dir = open("/data", O_RDONLY | O_DIRECTORY);
    let size = |fd, flags| statx(fd, b"\0", flags, 0x7ff).map(|s| s.size());
    check!(size(file, AT_EMPTY_PATH) == Ok(11), "a /data descriptor");
    check!(
        size(snapshot, AT_EMPTY_PATH) == Ok(3),
        "a snapshot descriptor"
    );
    check!(
        statx(dir, b"\0", AT_EMPTY_PATH, 0x7ff).map(|s| s.mode() & 0o170000) == Ok(S_IFDIR),
        "a directory descriptor"
    );
    check!(
        statx(dir, &cstr("s"), 0, 0x7ff).map(|s| s.size()) == Ok(11),
        "a name relative to a directory descriptor"
    );
    check!(
        statx(dir, &cstr("/tmp/t"), 0, 0x7ff).map(|s| s.size()) == Ok(3),
        "an absolute path ignores the descriptor"
    );
    check!(
        statx(AT_FDCWD, &cstr("data/s"), 0, 0x7ff).map(|s| s.size()) == Ok(11),
        "a relative path from the cwd"
    );
    check!(
        statx(dir, &cstr("s"), AT_EMPTY_PATH, 0x7ff).map(|s| s.size()) == Ok(11),
        "AT_EMPTY_PATH with a non-empty path looks the path up"
    );
    for fd in [file, snapshot, dir] {
        close(fd);
    }
    check!(path_call(SYS_UNLINK, "/data/s", 0) == 0, "unlink failed");
    check!(
        path_call(SYS_UNLINK, "/tmp/t", 0) == 0,
        "unlink /tmp/t failed"
    );
    data.check_clean()
}

/// Flags that change nothing here are accepted; unknown or contradictory ones,
/// reserved mask bits, missing names and bad descriptors are refused.
pub fn statx_flags_mask_and_errors() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/s", b"hello")?;
    let path = cstr("/data/s");
    for flags in [
        AT_SYMLINK_NOFOLLOW,
        AT_NO_AUTOMOUNT,
        AT_STATX_FORCE_SYNC,
        AT_STATX_DONT_SYNC,
        AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT | AT_STATX_DONT_SYNC,
    ] {
        check!(
            statx(AT_FDCWD, &path, flags, 0x7ff).map(|s| s.size()) == Ok(5),
            "flags {flags:#x} were refused"
        );
    }
    // Mask bits only say what the caller wants; any subset gets the same reply.
    for mask in [0, 0x1, 0x200, 0xfff, 0x7fff_ffff] {
        check!(
            statx(AT_FDCWD, &path, 0, mask).map(|s| s.mask()) == Ok(STATX_CLAIMED),
            "mask {mask:#x}"
        );
    }

    let refused = |result: Result<Statx, u64>| result.err();
    for flags in [
        AT_STATX_FORCE_SYNC | AT_STATX_DONT_SYNC,
        0x1,
        0x10,
        0x8000,
        1 << 20,
    ] {
        check!(
            refused(statx(AT_FDCWD, &path, flags, 0x7ff)) == Some(errno(EINVAL)),
            "flags {flags:#x} were accepted"
        );
    }
    check!(
        refused(statx(AT_FDCWD, &path, 0, STATX_RESERVED)) == Some(errno(EINVAL)),
        "the reserved mask bit was accepted"
    );
    check!(
        refused(statx(AT_FDCWD, b"\0", 0, 0x7ff)) == Some(errno(ENOENT)),
        "an empty path without AT_EMPTY_PATH"
    );
    check!(
        refused(statx(AT_FDCWD, &cstr("/data/nope"), 0, 0x7ff)) == Some(errno(ENOENT)),
        "a missing file"
    );
    check!(
        refused(statx(99, &cstr("s"), 0, 0x7ff)) == Some(errno(EBADF)),
        "a relative path from a closed descriptor"
    );
    check!(
        refused(statx(99, b"\0", AT_EMPTY_PATH, 0x7ff)) == Some(errno(EBADF)),
        "AT_EMPTY_PATH on a closed descriptor"
    );
    check!(
        refused(statx(u64::MAX, b"\0", AT_EMPTY_PATH, 0x7ff)) == Some(errno(EBADF)),
        "AT_EMPTY_PATH on descriptor -1"
    );
    // No NUL within PATH_MAX bytes.
    let long = vec![b'a'; 5000];
    check!(
        refused(statx(AT_FDCWD, &long, 0, 0x7ff)) == Some(errno(ENAMETOOLONG)),
        "an unterminated path"
    );
    check!(path_call(SYS_UNLINK, "/data/s", 0) == 0, "unlink failed");
    data.check_clean()
}

/// With real user memory: a bad path or output pointer is `EFAULT`, and a
/// failed lookup leaves the caller's buffer untouched.
pub fn statx_bad_pointers() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/s", b"hello")?;
    let (path_at, buf_at) = (SPACE, SPACE + 0x1000);
    in_space(|| -> Result<(), String> {
        let _strict = Strict::on();
        let path = cstr("/data/s");
        // Safety: `in_space` mapped SPACE..; the writes stay in its first page.
        unsafe { core::ptr::copy_nonoverlapping(path.as_ptr(), path_at as *mut u8, path.len()) };
        let call = |path: u64, buf: u64, flags: u64| {
            process::linux::dispatch_args5_for_test(SYS_STATX, AT_FDCWD, path, flags, 0x7ff, buf)
        };
        let reply = || {
            let mut copy = [0u8; 256];
            // Safety: `in_space` mapped SPACE.., and the reply is 256 bytes.
            unsafe { core::ptr::copy_nonoverlapping(buf_at as *const u8, copy.as_mut_ptr(), 256) };
            copy
        };
        check!(call(path_at, buf_at, 0) == 0, "a mapped path and buffer");
        check!(reply()[40] == 5, "the reply was not written");

        for bad in [0xdead_0000u64, 0, u64::MAX - 3] {
            let code = call(bad, buf_at, 0);
            check!(code == errno(EFAULT), "a path at {bad:#x} -> {code:#x}");
            let code = call(path_at, bad, 0);
            check!(code == errno(EFAULT), "a buffer at {bad:#x} -> {code:#x}");
        }
        // A missing file writes nothing to the buffer.
        // Safety: as above.
        unsafe { core::ptr::write_bytes(buf_at as *mut u8, 0x5A, 256) };
        // Safety: as above.
        unsafe { (path_at as *mut u8).add(6).write(b'X') }; // "/data/X"
        check!(call(path_at, buf_at, 0) == errno(ENOENT), "a missing file");
        check!(
            reply().iter().all(|b| *b == 0x5A),
            "a failed statx wrote the buffer"
        );
        Ok(())
    })?;
    check!(path_call(SYS_UNLINK, "/data/s", 0) == 0, "unlink failed");
    data.check_clean()
}
