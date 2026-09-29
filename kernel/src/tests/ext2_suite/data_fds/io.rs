//! Reading, writing, seeking and the positional calls on `/data` descriptors.

use super::*;

/// Write, seek, read back, see another descriptor's writes immediately, sync,
/// remount, and find the data on the "disk".
pub fn roundtrip_and_persist() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open("/data/hello.txt", O_CREAT | O_RDWR);
    check!(fd >= 3 && fd < 16, "create returned {fd:#x}");
    check!(
        task::fd_kind(fd as usize) == task::FdKind::Vfs,
        "a /data file is not a VFS-backed descriptor"
    );
    check!(write(fd, b"hello world") == 11, "write did not report 11");
    check!(fstat_size(fd)? == 11, "fstat did not see the write");
    check!(lseek(fd, 0, SEEK_SET) == 0, "seek to start failed");
    check!(read(fd, 5) == Ok(b"hello".to_vec()), "first read differs");
    check!(
        read(fd, 100) == Ok(b" world".to_vec()),
        "second read differs"
    );
    check!(
        read(fd, 100) == Ok(Vec::new()),
        "a read at EOF was not empty"
    );
    check!(lseek(fd, -5, SEEK_END) == 6, "SEEK_END position wrong");
    check!(
        read(fd, 100) == Ok(b"world".to_vec()),
        "read from SEEK_END differs"
    );

    // A second open sees the first one's bytes with no close in between: there
    // is one file, not one snapshot per descriptor.
    let other = open("/data/hello.txt", O_RDONLY);
    check!(
        read(other, 100) == Ok(b"hello world".to_vec()),
        "second opener differs"
    );
    check!(lseek(fd, 0, SEEK_END) == 11, "seek to end failed");
    check!(write(fd, b"!") == 1, "append write failed");
    check!(
        read(other, 100) == Ok(b"!".to_vec()),
        "a later write is invisible to another fd"
    );
    close(other);
    close(fd);

    check!(
        path_call(SYS_UNLINK, "/nope", 0) == errno(ENOENT),
        "unlink of a missing file"
    );
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync failed");
    data.remount()?;
    check!(
        slurp("/data/hello.txt")? == b"hello world!",
        "data did not survive the remount"
    );
    data.check_clean()
}

/// `O_CREAT`, `O_EXCL`, `O_TRUNC`, `O_APPEND`, directories, and `F_GETFL`.
pub fn open_flags() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/f", b"0123456789")?;
    check!(
        open("/data/f", O_CREAT | O_EXCL | O_RDWR) == errno(EEXIST),
        "O_EXCL on an existing file"
    );
    check!(
        open("/data/missing", O_RDONLY) == errno(ENOENT),
        "open without O_CREAT"
    );
    check!(
        open("/data", O_WRONLY) == errno(EISDIR),
        "a directory opened for writing"
    );

    let fd = open("/data/f", O_WRONLY | O_APPEND);
    check!(
        syscall(SYS_FCNTL, fd, F_GETFL, 0, 0) == O_WRONLY | O_APPEND,
        "F_GETFL does not report O_WRONLY|O_APPEND"
    );
    check!(lseek(fd, 0, SEEK_SET) == 0, "seek failed");
    check!(write(fd, b"AB") == 2, "append write failed");
    check!(write(fd, b"C") == 1, "second append write failed");
    check!(
        read(fd, 4) == Err(errno(EBADF)),
        "read on a write-only descriptor"
    );
    close(fd);
    check!(
        slurp("/data/f")? == b"0123456789ABC",
        "O_APPEND did not write at the end"
    );

    let fd = open("/data/f", O_RDONLY);
    check!(
        write(fd, b"x") == errno(EBADF),
        "write on a read-only descriptor"
    );
    close(fd);

    let fd = open("/data/f", O_WRONLY | O_TRUNC);
    check!(fstat_size(fd)? == 0, "O_TRUNC left bytes behind");
    close(fd);
    check!(slurp("/data/f")?.is_empty(), "O_TRUNC file is not empty");
    check!(path_call(SYS_UNLINK, "/data/f", 0) == 0, "unlink failed");
    data.check_clean()
}

/// `lseek` edge cases: negative and past-the-end positions, bad whence, a
/// hole, and `dup` sharing one offset.
pub fn seek_bounds() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open("/data/s", O_CREAT | O_RDWR);
    check!(write(fd, b"abc") == 3, "write failed");
    check!(
        lseek(fd, -4, SEEK_END) == errno(EINVAL),
        "a seek before the start"
    );
    check!(
        lseek(fd, i64::MIN, SEEK_CUR) == errno(EINVAL),
        "an overflowing seek"
    );
    check!(lseek(fd, 0, 9) == errno(EINVAL), "an unknown whence");
    check!(lseek(fd, 1000, SEEK_SET) == 1000, "a seek past the end");
    check!(
        read(fd, 10) == Ok(Vec::new()),
        "a read past the end returned bytes"
    );
    check!(write(fd, b"z") == 1, "a write past the end failed");
    check!(fstat_size(fd)? == 1001, "the size after a sparse write");
    check!(
        pread(fd, 3, 500) == Ok(vec![0, 0, 0]),
        "the hole did not read as zeros"
    );

    // `dup` shares the open file description, so the offset moves for both.
    let twin = syscall(SYS_DUP, fd, 0, 0, 0);
    check!(twin != fd && twin < 16, "dup returned {twin:#x}");
    check!(lseek(fd, 0, SEEK_SET) == 0, "seek failed");
    check!(
        read(twin, 1) == Ok(b"a".to_vec()),
        "read through the dup differs"
    );
    check!(
        lseek(fd, 0, SEEK_CUR) == 1,
        "the dup did not share the offset"
    );
    close(fd);
    check!(
        read(twin, 1) == Ok(b"b".to_vec()),
        "the dup died with the original"
    );
    close(twin);
    check!(path_call(SYS_UNLINK, "/data/s", 0) == 0, "unlink failed");
    data.check_clean()
}

/// `pread64`/`pwrite64` leave the offset alone; `O_APPEND` still appends.
pub fn positional_io() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open("/data/p", O_CREAT | O_RDWR);
    check!(write(fd, b"0123456789") == 10, "write failed");
    check!(pwrite(fd, b"AB", 3) == 2, "pwrite failed");
    check!(lseek(fd, 0, SEEK_CUR) == 10, "pwrite moved the offset");
    check!(pread(fd, 4, 2) == Ok(b"2AB5".to_vec()), "pread differs");
    check!(lseek(fd, 0, SEEK_CUR) == 10, "pread moved the offset");
    check!(
        pread(fd, 4, 8) == Ok(b"89".to_vec()),
        "pread near EOF differs"
    );
    check!(
        pread(fd, 4, 500) == Ok(Vec::new()),
        "pread past EOF returned bytes"
    );
    check!(pwrite(fd, b"Z", 14) == 1, "pwrite past EOF failed");
    check!(
        fstat_size(fd)? == 15,
        "pwrite past EOF did not extend the file"
    );
    check!(
        pread(fd, 5, 10) == Ok(vec![0, 0, 0, 0, b'Z']),
        "the gap did not read as zeros"
    );
    check!(write(fd, b"!") == 1, "write after the pwrites failed");
    check!(
        pread(fd, 1, 10) == Ok(b"!".to_vec()),
        "write used a moved offset"
    );
    close(fd);

    // Linux appends on an O_APPEND file whatever offset pwrite is given.
    let fd = open("/data/p", O_WRONLY | O_APPEND);
    check!(
        pwrite(fd, b"END", 0) == 3,
        "pwrite on an append file failed"
    );
    close(fd);
    let all = slurp("/data/p")?;
    check!(
        all.len() == 18 && all.ends_with(b"END"),
        "pwrite did not append: {all:?}"
    );
    check!(path_call(SYS_UNLINK, "/data/p", 0) == 0, "unlink failed");
    data.check_clean()
}

/// Errors of the positional calls: bad descriptors, access modes, offsets,
/// and descriptors that cannot seek.
pub fn positional_bad_inputs() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/q", b"data")?;
    check!(pread(99, 1, 0) == Err(errno(EBADF)), "pread on a bad fd");
    check!(pwrite(99, b"x", 0) == errno(EBADF), "pwrite on a bad fd");
    check!(
        pread(1, 1, 0) == Err(errno(ESPIPE)),
        "pread on the terminal"
    );

    let mut fds = [0i32; 2];
    check!(
        syscall(SYS_PIPE, fds.as_mut_ptr() as u64, 0, 0, 0) == 0,
        "pipe failed"
    );
    check!(
        pread(fds[0] as u64, 1, 0) == Err(errno(ESPIPE)),
        "pread on a pipe"
    );
    check!(
        pwrite(fds[1] as u64, b"x", 0) == errno(ESPIPE),
        "pwrite on a pipe"
    );
    close(fds[0] as u64);
    close(fds[1] as u64);

    let ro = open("/data/q", O_RDONLY);
    check!(
        pwrite(ro, b"x", 0) == errno(EBADF),
        "pwrite on a read-only fd"
    );
    check!(
        pread(ro, 1, u64::MAX) == Err(errno(EINVAL)),
        "a negative pread offset"
    );
    check!(
        pwrite(ro, b"x", 1 << 63) == errno(EINVAL),
        "a negative pwrite offset"
    );
    close(ro);
    let wo = open("/data/q", O_WRONLY);
    check!(
        pread(wo, 1, 0) == Err(errno(EBADF)),
        "pread on a write-only fd"
    );
    check!(pwrite(wo, b"", 0) == 0, "a zero-length pwrite");
    close(wo);
    check!(
        slurp("/data/q")? == b"data",
        "a refused call changed the file"
    );
    check!(path_call(SYS_UNLINK, "/data/q", 0) == 0, "unlink failed");
    data.check_clean()
}

/// The copy-up root and `/tmp` keep snapshot descriptors; they get the same
/// positional calls, and a write through them still reaches the file.
pub fn snapshot_positional_io() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open("/tmp/snap", O_CREAT | O_RDWR);
    check!(
        task::fd_kind(fd as usize) == task::FdKind::File,
        "a /tmp file is no longer a snapshot descriptor"
    );
    check!(write(fd, b"0123456789") == 10, "write failed");
    check!(pwrite(fd, b"AB", 3) == 2, "pwrite failed");
    check!(lseek(fd, 0, SEEK_CUR) == 10, "pwrite moved the offset");
    check!(pread(fd, 4, 2) == Ok(b"2AB5".to_vec()), "pread differs");
    check!(lseek(fd, 0, SEEK_CUR) == 10, "pread moved the offset");
    check!(pwrite(fd, b"Z", 12) == 1, "pwrite past EOF failed");
    check!(
        pread(fd, 4, 9) == Ok(vec![b'9', 0, 0, b'Z']),
        "the gap differs"
    );
    close(fd);
    check!(
        slurp("/tmp/snap")? == b"012AB56789\0\0Z",
        "the backing file differs"
    );
    check!(path_call(SYS_UNLINK, "/tmp/snap", 0) == 0, "unlink failed");
    data.check_clean()
}

/// A bad user buffer is `-EFAULT` and costs nothing: the offset stays put and
/// the file is not touched (the bytes are staged before any state moves).
pub fn bad_user_buffers() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/b", b"abcdef")?;
    let fd = open("/data/b", O_RDWR);
    let previous = crate::user_ptr::set_trust_kernel_pointers(false);
    let unmapped = 0x10u64;
    let results = [
        syscall(SYS_READ, fd, unmapped, 4, 0),
        syscall(SYS_WRITE, fd, unmapped, 4, 0),
        syscall(SYS_PREAD, fd, unmapped, 4, 0),
        syscall(SYS_PWRITE, fd, unmapped, 4, 0),
        syscall(SYS_FSTATFS, fd, unmapped, 0, 0),
    ];
    crate::user_ptr::set_trust_kernel_pointers(previous);
    for (index, result) in results.iter().enumerate() {
        check!(
            *result == errno(EFAULT),
            "call {index} on a bad buffer returned {result:#x}"
        );
    }
    check!(
        lseek(fd, 0, SEEK_CUR) == 0,
        "a failed call moved the offset"
    );
    check!(fstat_size(fd)? == 6, "a failed write changed the size");
    close(fd);
    check!(
        slurp("/data/b")? == b"abcdef",
        "a failed write changed the bytes"
    );
    check!(path_call(SYS_UNLINK, "/data/b", 0) == 0, "unlink failed");
    data.check_clean()
}
