//! `ftruncate`/`truncate`, `fsync`/`fdatasync`/`syncfs`/`sync`, and `statfs`.

use super::*;

/// The descriptor's bytes for `len` bytes of a recognisable pattern.
fn pattern(len: usize) -> Vec<u8> {
    pattern_bytes(7, len)
}

/// Shrink and grow through a descriptor: the file follows, the offset does
/// not, growth reads as zeros, and the freed blocks return to the volume.
pub fn ftruncate_and_truncate() -> Result<(), String> {
    let data = Data::new(0)?;
    let (blocks_before, _) = free_space()?;
    let fd = open("/data/t", O_CREAT | O_RDWR);
    let full = pattern(5000);
    check!(write(fd, &full) == 5000, "write failed");
    let (blocks_full, _) = free_space()?;
    check!(blocks_full < blocks_before, "the write allocated no blocks");

    check!(
        syscall(SYS_FTRUNCATE, fd, 100, 0, 0) == 0,
        "ftruncate shrink failed"
    );
    check!(fstat_size(fd)? == 100, "size after shrinking");
    check!(lseek(fd, 0, SEEK_CUR) == 5000, "ftruncate moved the offset");
    check!(
        read(fd, 10) == Ok(Vec::new()),
        "a read past the new end returned bytes"
    );
    let (blocks_cut, _) = free_space()?;
    check!(blocks_cut > blocks_full, "shrinking did not free blocks");

    check!(
        syscall(SYS_FTRUNCATE, fd, 3000, 0, 0) == 0,
        "ftruncate grow failed"
    );
    check!(fstat_size(fd)? == 3000, "size after growing");
    check!(
        pread(fd, 100, 0) == Ok(full[..100].to_vec()),
        "the kept prefix changed"
    );
    let grown = pread(fd, 2900, 100).map_err(|code| format!("pread returned {code:#x}"))?;
    check!(
        grown.len() == 2900 && grown.iter().all(|&byte| byte == 0),
        "the grown range is not zero (stale data came back)"
    );
    check!(
        syscall(SYS_FTRUNCATE, fd, 0, 0, 0) == 0,
        "ftruncate to zero failed"
    );
    check!(
        free_space()?.0 == blocks_before,
        "truncating to zero leaked blocks"
    );
    close(fd);

    put("/data/t", &pattern(2500))?;
    check!(
        path_call(SYS_TRUNCATE, "/data/t", 10) == 0,
        "truncate by path failed"
    );
    check!(
        slurp("/data/t")? == pattern(10),
        "truncate by path left the wrong bytes"
    );
    check!(path_call(SYS_UNLINK, "/data/t", 0) == 0, "unlink failed");
    check!(
        free_space()?.0 == blocks_before,
        "the volume did not return to its baseline"
    );
    data.check_clean()
}

/// Bad descriptors, access modes and lengths.
pub fn truncate_bad_inputs() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/t", b"keep me")?;
    check!(
        syscall(SYS_FTRUNCATE, 99, 0, 0, 0) == errno(EBADF),
        "ftruncate on a bad fd"
    );
    let mut fds = [0i32; 2];
    check!(
        syscall(SYS_PIPE, fds.as_mut_ptr() as u64, 0, 0, 0) == 0,
        "pipe failed"
    );
    check!(
        syscall(SYS_FTRUNCATE, fds[1] as u64, 0, 0, 0) == errno(EINVAL),
        "ftruncate on a pipe"
    );
    close(fds[0] as u64);
    close(fds[1] as u64);

    let ro = open("/data/t", O_RDONLY);
    check!(
        syscall(SYS_FTRUNCATE, ro, 0, 0, 0) == errno(EINVAL),
        "ftruncate on a read-only descriptor"
    );
    check!(
        syscall(SYS_FTRUNCATE, ro, u64::MAX, 0, 0) == errno(EINVAL),
        "a negative length"
    );
    close(ro);

    check!(
        path_call(SYS_TRUNCATE, "/data/missing", 0) == errno(ENOENT),
        "truncate of a missing file"
    );
    check!(
        path_call(SYS_TRUNCATE, "/data", 0) == errno(EISDIR),
        "truncate of a directory"
    );
    check!(
        path_call(SYS_TRUNCATE, "/data/t", u64::MAX) == errno(EINVAL),
        "a negative truncate length"
    );
    check!(
        slurp("/data/t")? == b"keep me",
        "a refused call changed the file"
    );
    check!(path_call(SYS_UNLINK, "/data/t", 0) == 0, "unlink failed");
    data.check_clean()
}

/// Whether the disk's superblock currently says the volume is clean.
fn clean(data: &Data) -> bool {
    raw_state(data.disk) & 1 == 1
}

/// Every flush call makes the volume durable and clean; a write makes it dirty
/// again; descriptors with nothing to flush are refused.
pub fn fsync_and_sync_are_durable() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open("/data/d", O_CREAT | O_RDWR);
    check!(write(fd, b"one") == 3, "write failed");
    check!(!clean(&data), "a write left the volume marked clean");
    let flushes = data
        .disk
        .flushes
        .load(core::sync::atomic::Ordering::Relaxed);
    check!(syscall(SYS_FSYNC, fd, 0, 0, 0) == 0, "fsync failed");
    check!(clean(&data), "fsync did not leave the volume clean");
    check!(
        data.disk
            .flushes
            .load(core::sync::atomic::Ordering::Relaxed)
            > flushes,
        "fsync never reached the block device"
    );

    for (nr, name) in [(SYS_FDATASYNC, "fdatasync"), (SYS_SYNCFS, "syncfs")] {
        check!(write(fd, b"+") == 1, "write failed");
        check!(!clean(&data), "a write left the volume marked clean");
        check!(syscall(nr, fd, 0, 0, 0) == 0, "{name} failed");
        check!(clean(&data), "{name} did not leave the volume clean");
    }
    check!(write(fd, b"+") == 1, "write failed");
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync failed");
    check!(clean(&data), "sync did not leave the volume clean");
    close(fd);

    data.remount()?;
    check!(
        slurp("/data/d")? == b"one+++",
        "the synced bytes did not survive"
    );

    check!(
        syscall(SYS_FSYNC, 99, 0, 0, 0) == errno(EBADF),
        "fsync on a bad fd"
    );
    check!(
        syscall(SYS_SYNCFS, 99, 0, 0, 0) == errno(EBADF),
        "syncfs on a bad fd"
    );
    check!(
        syscall(SYS_FSYNC, 1, 0, 0, 0) == errno(EINVAL),
        "fsync on the terminal"
    );
    let mut fds = [0i32; 2];
    check!(
        syscall(SYS_PIPE, fds.as_mut_ptr() as u64, 0, 0, 0) == 0,
        "pipe failed"
    );
    check!(
        syscall(SYS_FSYNC, fds[0] as u64, 0, 0, 0) == errno(EINVAL),
        "fsync on a pipe"
    );
    close(fds[0] as u64);
    close(fds[1] as u64);
    // A snapshot descriptor's mount has nothing to flush, and that is success.
    let tmp = open("/tmp/x", O_CREAT | O_RDWR);
    check!(
        syscall(SYS_FSYNC, tmp, 0, 0, 0) == 0,
        "fsync on a /tmp file"
    );
    close(tmp);
    check!(path_call(SYS_UNLINK, "/tmp/x", 0) == 0, "unlink failed");
    check!(path_call(SYS_UNLINK, "/data/d", 0) == 0, "unlink failed");
    data.check_clean()
}

/// Read a `struct statfs` field (`index` counts 8-byte words).
fn word(stat: &[u8; 120], index: usize) -> u64 {
    u64::from_le_bytes(stat[index * 8..index * 8 + 8].try_into().unwrap())
}

/// `statfs`/`fstatfs` describe the volume, and its free space tracks writes.
pub fn statfs_reports_volume() -> Result<(), String> {
    let data = Data::new(0)?;
    let mut stat = [0u8; 120];
    check!(
        path_call(SYS_STATFS, "/data", stat.as_mut_ptr() as u64) == 0,
        "statfs(/data) failed"
    );
    check!(
        word(&stat, 0) == 0xEF53,
        "f_type is {:#x}, not ext2",
        word(&stat, 0)
    );
    check!(
        word(&stat, 1) == 1024 && word(&stat, 9) == 1024,
        "block size differs"
    );
    check!(
        word(&stat, 2) == u64::from(VOLUME_BLOCKS),
        "block count differs"
    );
    let free = word(&stat, 3);
    check!(
        free > 0 && free < word(&stat, 2),
        "free blocks out of range: {free}"
    );
    check!(word(&stat, 4) == free, "f_bavail differs from f_bfree");
    check!(
        word(&stat, 5) == 64 && word(&stat, 6) > 0,
        "inode counts differ"
    );
    check!(word(&stat, 8) == 255, "f_namelen differs");

    let fd = open("/data/big", O_CREAT | O_RDWR);
    check!(write(fd, &pattern(20 * 1024)) == 20 * 1024, "write failed");
    let mut after = [0u8; 120];
    check!(
        syscall(SYS_FSTATFS, fd, after.as_mut_ptr() as u64, 0, 0) == 0,
        "fstatfs failed"
    );
    check!(
        word(&after, 3) <= free - 20,
        "20 KiB of data did not use 20 blocks"
    );
    check!(
        word(&after, 6) == word(&stat, 6) - 1,
        "the file did not use an inode"
    );
    close(fd);
    check!(path_call(SYS_UNLINK, "/data/big", 0) == 0, "unlink failed");
    check!(free_space()?.0 == free, "the free space did not come back");

    // A path under /tmp is described by the ramfs there; a fabricated
    // directory by the root; a missing path and a bad fd fail.
    let mut tmp = [0u8; 120];
    check!(
        path_call(SYS_STATFS, "/tmp", tmp.as_mut_ptr() as u64) == 0,
        "statfs(/tmp) failed"
    );
    check!(
        word(&tmp, 0) == 0x8584_58f6,
        "f_type of /tmp is {:#x}",
        word(&tmp, 0)
    );
    check!(
        path_call(SYS_STATFS, "/bin", tmp.as_mut_ptr() as u64) == 0,
        "statfs of a fabricated directory"
    );
    check!(
        path_call(SYS_STATFS, "/data/none", tmp.as_mut_ptr() as u64) == errno(ENOENT),
        "statfs of a missing path"
    );
    check!(
        syscall(SYS_FSTATFS, 99, tmp.as_mut_ptr() as u64, 0, 0) == errno(EBADF),
        "fstatfs on a bad fd"
    );
    check!(
        syscall(SYS_FSTATFS, 1, tmp.as_mut_ptr() as u64, 0, 0) == errno(EINVAL),
        "fstatfs on the terminal"
    );
    data.check_clean()
}
