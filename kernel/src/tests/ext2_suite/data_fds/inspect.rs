//! What programs use to inspect the filesystem (issue #348): the synthetic
//! `/proc/mounts` family that `df` and `mount` read, `getdents` next to
//! `getdents64`, and `statx`.

use crate::tests::hardening_suite::{in_space, Strict, SPACE};

use super::*;

const SYS_STAT: u64 = 4;
const SYS_GETDENTS: u64 = 78;
const SYS_GETDENTS64: u64 = 217;

pub(super) const O_DIRECTORY: u64 = 0o200000;
const ENOTDIR: u64 = 20;

const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;

pub(super) const S_IFDIR: u32 = 0o040000;
pub(super) const S_IFREG: u32 = 0o100000;

// ---------------------------------------------------------------------------
// /proc/mounts
// ---------------------------------------------------------------------------

/// Every byte of `path`, as text.
fn read_text(path: &str) -> Result<String, String> {
    String::from_utf8(slurp(path)?).map_err(|_| format!("{path} is not UTF-8"))
}

fn mounts_text(data_access: &str) -> String {
    format!("ramfs / ramfs rw 0 0\nramfs /tmp ramfs rw 0 0\next2 /data ext2 {data_access} 0 0\n")
}

fn mountinfo_text(data_access: &str) -> String {
    format!(
        "1 1 0:1 / / rw - ramfs ramfs rw\n2 1 0:2 / /tmp rw - ramfs ramfs rw\n\
         3 1 0:3 / /data {data_access} - ext2 ext2 {data_access}\n"
    )
}

/// The `stat` buffer of `path`, or the raw error.
fn stat_path(path: &str) -> Result<[u8; 144], u64> {
    let mut stat = [0u8; 144];
    let ret = syscall(
        SYS_STAT,
        cstr(path).as_ptr() as u64,
        stat.as_mut_ptr() as u64,
        0,
        0,
    );
    if ret == 0 {
        Ok(stat)
    } else {
        Err(ret)
    }
}

fn stat_mode(stat: &[u8; 144]) -> u32 {
    u32::from_le_bytes(stat[24..28].try_into().unwrap())
}

/// `/proc/mounts`, `/proc/self/mounts` and `/proc/self/mountinfo` list the ABI
/// mount table with the right `rw`/`ro`, and are read-only regular files.
pub fn proc_mounts_lists_the_table() -> Result<(), String> {
    let data = Data::new(0)?;
    for path in ["/proc/mounts", "/proc/self/mounts"] {
        check!(read_text(path)? == mounts_text("rw"), "{path} differs");
    }
    check!(
        read_text("/proc/self/mountinfo")? == mountinfo_text("rw"),
        "mountinfo differs"
    );

    // Reads in small pieces reassemble the same text, and stat agrees.
    let fd = open("/proc/mounts", O_RDONLY);
    check!(fd < 16, "open returned {fd:#x}");
    check!(
        fstat_size(fd)? == mounts_text("rw").len() as u64,
        "fstat size"
    );
    let mut pieces = Vec::new();
    while let Ok(piece) = read(fd, 7) {
        if piece.is_empty() {
            break;
        }
        pieces.extend_from_slice(&piece);
    }
    check!(
        pieces == mounts_text("rw").as_bytes(),
        "piecewise read differs"
    );
    close(fd);
    let stat = stat_path("/proc/mounts").map_err(|code| format!("stat returned {code:#x}"))?;
    check!(stat_mode(&stat) == S_IFREG | 0o444, "mode of /proc/mounts");
    check!(
        stat_path("/proc/self").map(|s| stat_mode(&s)) == Ok(S_IFDIR | 0o755),
        "/proc/self is not a directory"
    );

    // Fabricated: nothing can be written or created through them.
    for flags in [O_RDWR, O_WRONLY | O_TRUNC, O_CREAT | O_WRONLY] {
        check!(
            open("/proc/mounts", flags) == errno(EROFS),
            "opening /proc/mounts with {flags:#o}"
        );
    }
    check!(
        open("/proc/self/nothing", O_RDONLY) == errno(ENOENT),
        "an unknown /proc file"
    );

    // A stranger may read them too.
    credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
    check!(
        read_text("/proc/mounts")? == mounts_text("rw"),
        "as a stranger"
    );
    credentials::set(task::current(), Cred::ROOT);
    data.check_clean()
}

/// A volume mounted read-only shows as `ro`.
pub fn proc_mounts_reports_a_read_only_volume() -> Result<(), String> {
    let data = Data::new(0)?;
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync failed");
    data.disk.set_read_only(true);
    data.remount()?;
    check!(read_text("/proc/mounts")? == mounts_text("ro"), "mounts");
    check!(
        read_text("/proc/self/mountinfo")? == mountinfo_text("ro"),
        "mountinfo"
    );
    data.check_clean()
}

// ---------------------------------------------------------------------------
// getdents / getdents64
// ---------------------------------------------------------------------------

/// One decoded directory record.
#[derive(Debug, PartialEq)]
struct Dirent {
    ino: u64,
    off: u64,
    reclen: usize,
    kind: u8,
    name: String,
}

/// Decode a `linux_dirent64` (`legacy == false`) or `linux_dirent` stream.
fn parse(bytes: &[u8], legacy: bool) -> Result<Vec<Dirent>, String> {
    let mut entries = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let rec = &bytes[at..];
        check!(rec.len() >= 24, "a truncated record at {at}");
        let reclen = usize::from(u16::from_le_bytes([rec[16], rec[17]]));
        check!(
            reclen % 8 == 0 && reclen <= rec.len(),
            "a bad record length {reclen} at {at}"
        );
        let (kind, name_at) = if legacy {
            (rec[reclen - 1], 18)
        } else {
            (rec[18], 19)
        };
        let name_len = rec[name_at..reclen]
            .iter()
            .position(|b| *b == 0)
            .ok_or("no NUL")?;
        entries.push(Dirent {
            ino: u64::from_le_bytes(rec[0..8].try_into().unwrap()),
            off: u64::from_le_bytes(rec[8..16].try_into().unwrap()),
            reclen,
            kind,
            name: String::from_utf8(rec[name_at..name_at + name_len].to_vec())
                .map_err(|_| "a name is not UTF-8")?,
        });
        at += reclen;
    }
    Ok(entries)
}

/// One `getdents`/`getdents64` call with a `count`-byte buffer: the bytes on
/// success, the raw return on error.
fn dents(nr: u64, fd: u64, count: usize) -> Result<Vec<u8>, u64> {
    let mut buf = vec![0xEEu8; count];
    let got = syscall(nr, fd, buf.as_mut_ptr() as u64, count as u64, 0);
    if got > count as u64 {
        return Err(got);
    }
    buf.truncate(got as usize);
    Ok(buf)
}

/// A directory with two short names, a long one and a subdirectory.
fn make_dir() -> Result<(), String> {
    check!(path_call(SYS_MKDIR, "/data/d", 0o755) == 0, "mkdir failed");
    put("/data/d/alpha", b"x")?;
    put("/data/d/b", b"y")?;
    put("/data/d/a_rather_long_file_name_0123456789", b"z")?;
    check!(path_call(SYS_MKDIR, "/data/d/sub", 0o755) == 0, "mkdir sub");
    Ok(())
}

/// Both layouts list the same entries, with the type where each layout puts it.
pub fn getdents_matches_getdents64() -> Result<(), String> {
    let data = Data::new(0)?;
    make_dir()?;
    let modern = open("/data/d", O_RDONLY | O_DIRECTORY);
    let legacy = open("/data/d", O_RDONLY | O_DIRECTORY);
    check!(modern < 16 && legacy < 16, "opening the directory");
    let new_stream = dents(SYS_GETDENTS64, modern, 4096).map_err(|c| format!("{c:#x}"))?;
    let old_stream = dents(SYS_GETDENTS, legacy, 4096).map_err(|c| format!("{c:#x}"))?;
    check!(
        new_stream.len() == old_stream.len(),
        "the streams differ in length"
    );
    let new = parse(&new_stream, false)?;
    let old = parse(&old_stream, true)?;
    check!(new == old, "the layouts disagree: {new:?} vs {old:?}");

    let mut names: Vec<&str> = new.iter().map(|entry| entry.name.as_str()).collect();
    names.sort_unstable();
    check!(
        names
            == [
                ".",
                "..",
                "a_rather_long_file_name_0123456789",
                "alpha",
                "b",
                "sub"
            ],
        "names: {names:?}"
    );
    for entry in &new {
        let want = if entry.name == "sub" || entry.name.starts_with('.') {
            DT_DIR
        } else {
            DT_REG
        };
        check!(
            entry.kind == want,
            "the type of {} is {}",
            entry.name,
            entry.kind
        );
    }
    // `d_off` is where the next record starts.
    let mut end = 0;
    for entry in &new {
        end += entry.reclen as u64;
        check!(entry.off == end, "d_off of {} is {}", entry.name, entry.off);
    }
    check!(
        dents(SYS_GETDENTS, legacy, 4096) == Ok(Vec::new()),
        "no end of directory"
    );
    close(modern);
    close(legacy);
    data.check_clean()
}

/// A small buffer gets whole records only, each call carries on where the last
/// stopped, and a buffer too small for the next record is `EINVAL`.
pub fn getdents_hands_out_whole_records() -> Result<(), String> {
    let data = Data::new(0)?;
    make_dir()?;
    let all = {
        let fd = open("/data/d", O_RDONLY);
        let stream = dents(SYS_GETDENTS64, fd, 4096).map_err(|c| format!("{c:#x}"))?;
        close(fd);
        parse(&stream, false)?
    };
    for nr in [SYS_GETDENTS, SYS_GETDENTS64] {
        let legacy = nr == SYS_GETDENTS;
        let fd = open("/data/d", O_RDONLY);
        check!(dents(nr, fd, 10) == Err(errno(EINVAL)), "a 10-byte buffer");
        check!(dents(nr, fd, 0) == Err(errno(EINVAL)), "a 0-byte buffer");
        let mut seen = Vec::new();
        loop {
            let chunk = dents(nr, fd, 64).map_err(|code| format!("call returned {code:#x}"))?;
            if chunk.is_empty() {
                break;
            }
            check!(
                chunk.len() <= 64,
                "{} bytes in a 64-byte buffer",
                chunk.len()
            );
            seen.extend(parse(&chunk, legacy)?);
        }
        check!(seen == all, "piecewise listing differs for syscall {nr}");

        // `seekdir`: a `d_off` is a valid position to resume from.
        let resume = all[1].off;
        check!(
            lseek(fd, resume as i64, SEEK_SET) == resume,
            "seek to d_off"
        );
        let rest = dents(nr, fd, 4096).map_err(|code| format!("{code:#x}"))?;
        check!(parse(&rest, legacy)? == all[2..], "listing after seekdir");
        close(fd);
    }
    data.check_clean()
}

/// Descriptors that are not directory streams are refused.
pub fn getdents_bad_descriptors() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/f", b"file")?;
    put("/tmp/f", b"file")?;
    for nr in [SYS_GETDENTS, SYS_GETDENTS64] {
        let on_data = open("/data/f", O_RDONLY);
        let on_tmp = open("/tmp/f", O_RDONLY);
        check!(
            dents(nr, on_data, 4096) == Err(errno(ENOTDIR)),
            "a /data file"
        );
        check!(
            dents(nr, on_tmp, 4096) == Err(errno(ENOTDIR)),
            "a snapshot file"
        );
        check!(dents(nr, 99, 4096) == Err(errno(EBADF)), "a closed fd");
        check!(dents(nr, 1, 4096) == Err(errno(EBADF)), "the terminal");
        close(on_data);
        close(on_tmp);
    }
    data.check_clean()
}

/// A buffer that cannot be written is `EFAULT` and loses nothing: the next
/// call, with a good buffer, still returns the first entries.
pub fn getdents_bad_buffer_loses_nothing() -> Result<(), String> {
    let data = Data::new(0)?;
    make_dir()?;
    let fd = open("/data/d", O_RDONLY);
    in_space(|| -> Result<(), String> {
        let _strict = Strict::on();
        for nr in [SYS_GETDENTS, SYS_GETDENTS64] {
            for buf in [0xdead_0000u64, 0, u64::MAX - 3] {
                let code = syscall(nr, fd, buf, 4096, 0);
                check!(
                    code == errno(EFAULT),
                    "syscall {nr} into {buf:#x} -> {code:#x}"
                );
            }
        }
        // A good user buffer works, and starts at the first record.
        let got = syscall(SYS_GETDENTS64, fd, SPACE, 4096, 0);
        check!(got > 0 && got <= 4096, "a mapped buffer returned {got:#x}");
        // Safety: `in_space` mapped SPACE, and the call just filled `got` bytes.
        let bytes = unsafe { core::slice::from_raw_parts(SPACE as *const u8, got as usize) };
        let entries = parse(bytes, false)?;
        check!(
            entries.first().map(|e| e.name.as_str()) == Some("."),
            "not from the start"
        );
        Ok(())
    })?;
    close(fd);
    data.check_clean()
}
