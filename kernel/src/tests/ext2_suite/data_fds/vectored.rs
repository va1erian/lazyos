//! `preadv`, `pwritev` and their `*2` forms (issue #348): the positional
//! vectored calls share `readv`/`writev`'s bounds checks, so a hostile
//! `iovec` count, length or array address must stay refused here too.

use super::*;
use crate::tests::hardening_suite::Strict;

const SYS_READV: u64 = 19;
const SYS_WRITEV: u64 = 20;
const SYS_PREADV: u64 = 295;
const SYS_PWRITEV: u64 = 296;
const SYS_PREADV2: u64 = 327;
const SYS_PWRITEV2: u64 = 328;

const EOPNOTSUPP: u64 = 95;

const RWF_HIPRI: u64 = 0x1;
const RWF_DSYNC: u64 = 0x2;
const RWF_SYNC: u64 = 0x4;
const RWF_NOWAIT: u64 = 0x8;
const RWF_APPEND: u64 = 0x10;

/// Offset argument that makes the `*2` calls use the descriptor position.
const CURRENT: u64 = u64::MAX;

/// Cycles of the soak loop.
const CYCLES: usize = 300;

/// An `iovec` array over `bufs`: `[base, len]` word pairs.
fn iovecs<'a>(bufs: impl IntoIterator<Item = &'a [u8]>) -> Vec<u64> {
    let mut words = Vec::new();
    for buf in bufs {
        words.push(buf.as_ptr() as u64);
        words.push(buf.len() as u64);
    }
    words
}

/// A vectored call over the `iovec` array `words` (see [`iovecs`]). The
/// offset and flags go where `preadv2`/`pwritev2` take them (`pos_l`, `flags`).
fn vectored(nr: u64, fd: u64, words: &[u64], offset: u64, flags: u64) -> u64 {
    raw(
        nr,
        fd,
        words.as_ptr() as u64,
        words.len() as u64 / 2,
        offset,
        flags,
    )
}

/// A vectored call with every argument spelled out (hostile ones included).
fn raw(nr: u64, fd: u64, iov: u64, count: u64, offset: u64, flags: u64) -> u64 {
    process::linux::dispatch_args6_for_test(nr, [fd, iov, count, offset, 0, flags])
}

/// `pwritev(fd, chunks, offset)`.
fn pwritev(fd: u64, chunks: &[&[u8]], offset: u64) -> u64 {
    vectored(SYS_PWRITEV, fd, &iovecs(chunks.iter().copied()), offset, 0)
}

/// A vectored read into buffers of the given sizes: the bytes received
/// (concatenated) on success, the raw return on error.
fn read_into(nr: u64, fd: u64, sizes: &[usize], offset: u64, flags: u64) -> Result<Vec<u8>, u64> {
    let mut bufs: Vec<Vec<u8>> = sizes.iter().map(|len| vec![0xEEu8; *len]).collect();
    let words = iovecs(bufs.iter().map(|buf| &buf[..]));
    let got = vectored(nr, fd, &words, offset, flags);
    if got > sizes.iter().sum::<usize>() as u64 {
        return Err(got);
    }
    let mut joined: Vec<u8> = bufs.iter_mut().flat_map(|buf| buf.drain(..)).collect();
    joined.truncate(got as usize);
    Ok(joined)
}

/// `preadv(fd, sizes, offset)`.
fn preadv(fd: u64, sizes: &[usize], offset: u64) -> Result<Vec<u8>, u64> {
    read_into(SYS_PREADV, fd, sizes, offset, 0)
}

/// Positional vectored I/O moves the bytes to the right places, leaves the
/// descriptor offset alone, and behaves at and past end of file.
pub fn preadv_pwritev_roundtrip() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open("/data/v", O_CREAT | O_RDWR);
    check!(write(fd, b"0123456789") == 10, "write failed");
    check!(
        pwritev(fd, &[b"AB", b"", b"CDE"], 3) == 5,
        "pwritev over the middle"
    );
    check!(lseek(fd, 0, SEEK_CUR) == 10, "pwritev moved the offset");
    check!(
        slurp("/data/v")? == b"012ABCDE89",
        "pwritev put the bytes elsewhere"
    );
    check!(
        preadv(fd, &[2, 3, 4], 2) == Ok(b"2ABCDE89".to_vec()),
        "preadv across three segments"
    );
    check!(
        preadv(fd, &[3, 2], 1) == Ok(b"12ABC".to_vec()),
        "preadv across two segments"
    );
    check!(lseek(fd, 0, SEEK_CUR) == 10, "preadv moved the offset");

    // A short read ends the walk: later segments are not filled.
    check!(
        preadv(fd, &[4, 4], 7) == Ok(b"E89".to_vec()),
        "preadv near EOF"
    );
    for at in [10, 5000, i64::MAX as u64] {
        check!(
            preadv(fd, &[4], at) == Ok(Vec::new()),
            "preadv at {at:#x} returned bytes"
        );
    }
    check!(
        vectored(SYS_PREADV, fd, &[], 0, 0) == 0,
        "preadv with no segments"
    );

    // Writing past the end leaves a hole and extends the file.
    check!(pwritev(fd, &[b"xy", b"z"], 14) == 3, "pwritev past EOF");
    check!(fstat_size(fd)? == 17, "size after pwritev past EOF");
    check!(
        preadv(fd, &[6], 10) == Ok(vec![0, 0, 0, 0, b'x', b'y']),
        "the hole did not read as zeros"
    );
    close(fd);
    check!(path_call(SYS_UNLINK, "/data/v", 0) == 0, "unlink failed");
    data.check_clean()
}

/// `readv`/`writev` (which now share the walk with the positional forms) still
/// move the descriptor offset and stop at a short transfer.
pub fn readv_writev_share_the_walk() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open("/data/w", O_CREAT | O_RDWR);
    let words = iovecs([&b"abc"[..], &b"defg"[..]]);
    check!(vectored(SYS_WRITEV, fd, &words, 0, 0) == 7, "writev");
    check!(lseek(fd, 0, SEEK_CUR) == 7, "writev did not advance");
    check!(lseek(fd, 0, SEEK_SET) == 0, "seek failed");
    check!(
        read_into(SYS_READV, fd, &[4, 2], 0, 0) == Ok(b"abcdef".to_vec()),
        "readv across two segments"
    );
    check!(
        read_into(SYS_READV, fd, &[4, 4], 0, 0) == Ok(b"g".to_vec()),
        "readv did not stop at the short read"
    );
    close(fd);
    check!(path_call(SYS_UNLINK, "/data/w", 0) == 0, "unlink failed");
    data.check_clean()
}

/// Bad descriptors, access modes, offsets and non-seekable descriptors, and
/// hostile counts, lengths and array addresses.
pub fn vectored_bad_inputs() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/q", b"data")?;
    let one = iovecs([&b"x"[..]]);
    for nr in [SYS_PREADV, SYS_PWRITEV] {
        check!(
            vectored(nr, 99, &one, 0, 0) == errno(EBADF),
            "syscall {nr} on a bad fd"
        );
        check!(
            vectored(nr, 1, &one, 0, 0) == errno(ESPIPE),
            "syscall {nr} on the terminal"
        );
    }

    let mut fds = [0i32; 2];
    check!(
        syscall(SYS_PIPE, fds.as_mut_ptr() as u64, 0, 0, 0) == 0,
        "pipe failed"
    );
    for (nr, end) in [(SYS_PREADV, fds[0]), (SYS_PWRITEV, fds[1])] {
        check!(
            vectored(nr, end as u64, &one, 0, 0) == errno(ESPIPE),
            "syscall {nr} on a pipe"
        );
    }
    close(fds[0] as u64);
    close(fds[1] as u64);

    let ro = open("/data/q", O_RDONLY);
    let wo = open("/data/q", O_WRONLY);
    check!(
        vectored(SYS_PWRITEV, ro, &one, 0, 0) == errno(EBADF),
        "pwritev on a read-only fd"
    );
    check!(
        vectored(SYS_PREADV, wo, &one, 0, 0) == errno(EBADF),
        "preadv on a write-only fd"
    );
    for negative in [u64::MAX, 1 << 63, (-2i64) as u64] {
        for (nr, fd) in [(SYS_PREADV, ro), (SYS_PWRITEV, wo)] {
            check!(
                vectored(nr, fd, &one, negative, 0) == errno(EINVAL),
                "syscall {nr} at offset {negative:#x}"
            );
        }
    }

    // Counts past the cap are refused before any entry is read; lengths past
    // `isize::MAX` are refused when their entry is.
    for count in [1025u64, 1 << 32, u64::MAX] {
        for (nr, fd) in [(SYS_PREADV, ro), (SYS_PWRITEV, wo)] {
            check!(
                raw(nr, fd, one.as_ptr() as u64, count, 0, 0) == errno(EINVAL),
                "syscall {nr} with count {count:#x}"
            );
        }
    }
    for len in [u64::MAX, 1 << 63] {
        let huge = [one[0], len];
        for (nr, fd) in [(SYS_PREADV, ro), (SYS_PWRITEV, wo)] {
            check!(
                vectored(nr, fd, &huge, 0, 0) == errno(EINVAL),
                "syscall {nr} with a segment of {len:#x} bytes"
            );
        }
    }
    hostile_arrays(ro, wo)?;
    close(ro);
    close(wo);
    check!(
        slurp("/data/q")? == b"data",
        "a refused call changed the file"
    );
    check!(path_call(SYS_UNLINK, "/data/q", 0) == 0, "unlink failed");
    data.check_clean()
}

/// Arrays that are unmapped, or whose entries wrap past the address space, are
/// `-EFAULT` (with pointer validation on, as for a real user task).
fn hostile_arrays(readable: u64, writable: u64) -> Result<(), String> {
    let _strict = Strict::on();
    for array in [0xdead_0000u64, 0, u64::MAX - 15, u64::MAX - 8, u64::MAX] {
        for count in [1u64, 2, 1024] {
            for (nr, fd) in [(SYS_PREADV, readable), (SYS_PWRITEV, writable)] {
                let code = raw(nr, fd, array, count, 0, 0);
                check!(
                    code == errno(EFAULT),
                    "syscall {nr} on array {array:#x} x{count} -> {code:#x}"
                );
            }
        }
    }
    Ok(())
}

/// `preadv2`/`pwritev2`: an offset of -1 is `readv`/`writev` at the descriptor
/// position, scheduling hints are accepted, and flags that would change what
/// is written or when it is durable are refused rather than dropped.
pub fn vectored_v2_flags_and_current_offset() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/t", b"0123456789")?;
    let fd = open("/data/t", O_RDWR);
    check!(
        read_into(SYS_PREADV2, fd, &[2, 1], CURRENT, 0) == Ok(b"012".to_vec()),
        "preadv2 at the current position"
    );
    check!(lseek(fd, 0, SEEK_CUR) == 3, "preadv2 -1 did not advance");
    check!(
        read_into(SYS_PREADV2, fd, &[2], 5, 0) == Ok(b"56".to_vec()),
        "preadv2 at an offset"
    );
    check!(lseek(fd, 0, SEEK_CUR) == 3, "preadv2 moved the position");

    let words = iovecs([&b"a"[..], &b"b"[..]]);
    check!(
        vectored(SYS_PWRITEV2, fd, &words, CURRENT, 0) == 2,
        "pwritev2 at the current position"
    );
    check!(lseek(fd, 0, SEEK_CUR) == 5, "pwritev2 -1 did not advance");
    check!(
        vectored(SYS_PWRITEV2, fd, &iovecs([&b"Z"[..]]), 9, 0) == 1,
        "pwritev2 at an offset"
    );
    check!(lseek(fd, 0, SEEK_CUR) == 5, "pwritev2 moved the position");
    check!(
        slurp("/data/t")? == b"012ab5678Z",
        "contents after pwritev2"
    );

    for hints in [RWF_HIPRI, RWF_NOWAIT, RWF_HIPRI | RWF_NOWAIT] {
        check!(
            vectored(SYS_PWRITEV2, fd, &words, 0, hints) == 2,
            "pwritev2 with hints {hints:#x}"
        );
        check!(
            read_into(SYS_PREADV2, fd, &[2], 0, hints) == Ok(b"ab".to_vec()),
            "preadv2 with hints {hints:#x}"
        );
    }
    for refused in [RWF_DSYNC, RWF_SYNC, RWF_APPEND, RWF_HIPRI | RWF_SYNC, 0x100] {
        check!(
            vectored(SYS_PWRITEV2, fd, &iovecs([&b"!"[..]]), 0, refused) == errno(EOPNOTSUPP),
            "pwritev2 with flags {refused:#x}"
        );
        check!(
            read_into(SYS_PREADV2, fd, &[1], 0, refused) == Err(errno(EOPNOTSUPP)),
            "preadv2 with flags {refused:#x}"
        );
    }
    check!(
        vectored(SYS_PREADV2, fd, &words, (-2i64) as u64, 0) == errno(EINVAL),
        "preadv2 at offset -2"
    );
    check!(slurp("/data/t")? == b"ab2ab5678Z", "a refused call wrote");
    close(fd);
    check!(path_call(SYS_UNLINK, "/data/t", 0) == 0, "unlink failed");
    data.check_clean()
}

/// On a read-only volume a positional vectored write is `EROFS`, changes
/// nothing, and the read side still works.
pub fn vectored_on_read_only_volume() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/ro", b"fixed")?;
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync failed");
    data.disk.set_read_only(true);
    data.remount()?;

    let fd = open("/data/ro", O_RDWR);
    check!(fd < 16, "opening for write returned {fd:#x}");
    check!(
        pwritev(fd, &[b"x", b"y"], 0) == errno(EROFS),
        "pwritev on a read-only volume"
    );
    check!(
        vectored(SYS_PWRITEV2, fd, &iovecs([&b"x"[..]]), 0, 0) == errno(EROFS),
        "pwritev2 on a read-only volume"
    );
    check!(
        preadv(fd, &[2, 3], 0) == Ok(b"fixed".to_vec()),
        "preadv on a read-only volume"
    );
    close(fd);
    check!(slurp("/data/ro")? == b"fixed", "a refused write changed it");
    data.check_clean()
}

/// The same calls on a snapshot descriptor (the copy-up root and `/tmp`).
pub fn snapshot_vectored_io() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/tmp/v", b"0123456789")?;
    let fd = open("/tmp/v", O_RDWR);
    check!(
        pwritev(fd, &[b"AB", b"CDE"], 3) == 5,
        "pwritev on a snapshot"
    );
    check!(lseek(fd, 0, SEEK_CUR) == 0, "pwritev moved the offset");
    check!(
        preadv(fd, &[3, 9], 1) == Ok(b"12ABCDE89".to_vec()),
        "preadv on a snapshot"
    );
    check!(lseek(fd, 0, SEEK_CUR) == 0, "preadv moved the offset");
    check!(
        preadv(fd, &[2], 500) == Ok(Vec::new()),
        "preadv past EOF on a snapshot"
    );
    check!(
        pwritev(fd, &[b"q"], 12) == 1,
        "pwritev past EOF on a snapshot"
    );
    check!(
        preadv(fd, &[4], 9) == Ok(vec![b'9', 0, 0, b'q']),
        "the hole on a snapshot"
    );
    close(fd);
    check!(path_call(SYS_UNLINK, "/tmp/v", 0) == 0, "unlink failed");
    data.check_clean()
}

/// Many create/`pwritev`/`preadv`/unlink cycles, each with a refused hostile
/// call mixed in: no descriptor, registered open file, block or inode leaks.
pub fn soak_vectored_io() -> Result<(), String> {
    let data = Data::new(0)?;
    let baseline = free_space()?;
    for i in 0..CYCLES {
        let path = format!("/data/x{}", i % 4);
        let body = pattern_bytes(i as u32, 300 + (i % 5) * 900);
        let (a, rest) = body.split_at(body.len() / 3);
        let (b, c) = rest.split_at(rest.len() / 2);
        let fd = open(&path, O_CREAT | O_TRUNC | O_RDWR);
        check!(fd < 16, "cycle {i}: create returned {fd:#x}");
        check!(
            pwritev(fd, &[a, b, c], 0) == body.len() as u64,
            "cycle {i}: pwritev"
        );
        check!(
            raw(SYS_PWRITEV, fd, 0xdead_0000, 1025, 0, 0) == errno(EINVAL),
            "cycle {i}: a hostile count was not refused"
        );
        check!(
            preadv(fd, &[c.len(), b.len(), a.len()], 0) == Ok(body.clone()),
            "cycle {i}: preadv differs"
        );
        check!(lseek(fd, 0, SEEK_CUR) == 0, "cycle {i}: offset moved");
        if i % 3 == 0 {
            check!(path_call(SYS_UNLINK, &path, 0) == 0, "cycle {i}: unlink");
            check!(
                preadv(fd, &[body.len()], 0) == Ok(body.clone()),
                "cycle {i}: orphan unreadable"
            );
            check!(close(fd) == 0, "cycle {i}: close");
        } else {
            check!(close(fd) == 0, "cycle {i}: close");
            check!(path_call(SYS_UNLINK, &path, 0) == 0, "cycle {i}: unlink");
        }
    }
    check!(
        free_space()? == baseline,
        "blocks or inodes leaked over {CYCLES} cycles"
    );
    data.check_clean()
}
