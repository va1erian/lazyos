//! What happens to an open descriptor when its file is unlinked or renamed,
//! and what it may do at all (permissions, a read-only volume).

use super::*;

/// `rename(from, to)`.
fn rename(from: &str, to: &str) -> u64 {
    let (from, to) = (cstr(from), cstr(to));
    syscall(SYS_RENAME, from.as_ptr() as u64, to.as_ptr() as u64, 0, 0)
}

/// Whether a name in `/data` is the hidden entry of an unlinked-but-open file.
fn parked(names: &[String]) -> usize {
    names
        .iter()
        .filter(|name| name.starts_with(".unlinked-"))
        .count()
}

/// POSIX unlink: the name goes at once, the data lives until the last
/// descriptor (of any open) closes, and only then are the blocks freed.
pub fn unlink_while_open() -> Result<(), String> {
    let data = Data::new(0)?;
    let baseline = free_space()?;
    let fd = open("/data/u", O_CREAT | O_RDWR);
    let second_open = open("/data/u", O_RDWR);
    check!(write(fd, b"orphan data") == 11, "write failed");
    check!(
        path_call(SYS_UNLINK, "/data/u", 0) == 0,
        "unlink of an open file failed"
    );
    check!(
        open("/data/u", O_RDONLY) == errno(ENOENT),
        "the name survived the unlink"
    );
    check!(
        parked(&data_names()?) == 1,
        "the open file was not parked under a hidden name"
    );

    check!(lseek(fd, 0, SEEK_SET) == 0, "seek failed");
    check!(
        read(fd, 100) == Ok(b"orphan data".to_vec()),
        "the data vanished with the name"
    );
    check!(write(fd, b"++") == 2, "a write to an unlinked file failed");
    check!(fstat_size(fd)? == 13, "fstat of an unlinked file");
    check!(
        syscall(SYS_FSYNC, fd, 0, 0, 0) == 0,
        "fsync of an unlinked file"
    );

    // The name is free again and names a different file.
    put("/data/u", b"second")?;
    check!(
        slurp("/data/u")? == b"second",
        "the new file at the old name differs"
    );
    check!(
        pread(fd, 6, 0) == Ok(b"orphan".to_vec()),
        "the old descriptor changed files"
    );

    let twin = syscall(SYS_DUP, fd, 0, 0, 0);
    close(fd);
    check!(
        syscall(SYS_FTRUNCATE, twin, 4, 0, 0) == 0,
        "ftruncate through a dup failed"
    );
    check!(
        pread(twin, 10, 0) == Ok(b"orph".to_vec()),
        "the dup lost the file"
    );
    close(twin);
    check!(
        pread(second_open, 10, 0) == Ok(b"orph".to_vec()),
        "the other open lost the file"
    );
    check!(
        parked(&data_names()?) == 1,
        "the data was freed while an open remained"
    );
    close(second_open);
    check!(
        parked(&data_names()?) == 0,
        "the hidden entry outlived the last close"
    );

    check!(path_call(SYS_UNLINK, "/data/u", 0) == 0, "unlink failed");
    check!(
        free_space()? == baseline,
        "unlinked blocks or inodes were not returned"
    );
    data.check_clean()
}

/// An open file follows its name through `rename`, including the rename of a
/// directory above it (and not that of a sibling with a longer name).
pub fn rename_while_open() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open("/data/a", O_CREAT | O_RDWR);
    check!(write(fd, b"A") == 1, "write failed");
    check!(rename("/data/a", "/data/b") == 0, "rename failed");
    check!(write(fd, b"B") == 1, "write after the rename failed");
    check!(
        slurp("/data/b")? == b"AB",
        "the write did not follow the rename"
    );
    check!(
        open("/data/a", O_RDONLY) == errno(ENOENT),
        "the old name survived"
    );
    close(fd);

    check!(path_call(SYS_MKDIR, "/data/d", 0o755) == 0, "mkdir failed");
    let inside = open("/data/d/f", O_CREAT | O_RDWR);
    let sibling = open("/data/dd", O_CREAT | O_RDWR);
    check!(
        rename("/data/d", "/data/e") == 0,
        "renaming the directory failed"
    );
    check!(
        write(inside, b"in") == 2,
        "write into the moved directory failed"
    );
    check!(write(sibling, b"sib") == 3, "write to the sibling failed");
    check!(
        slurp("/data/e/f")? == b"in",
        "the file did not move with its directory"
    );
    check!(
        slurp("/data/dd")? == b"sib",
        "a sibling with a longer name was retargeted"
    );
    close(inside);
    close(sibling);

    for path in ["/data/b", "/data/e/f", "/data/dd"] {
        check!(path_call(SYS_UNLINK, path, 0) == 0, "unlink({path}) failed");
    }
    check!(
        path_call(SYS_UNLINK, "/data/e", 0) != 0,
        "unlink removed a directory"
    );
    data.check_clean()
}

/// Renaming over an open file unlinks it (its descriptors keep the old data),
/// and a rename that then fails puts everything back.
pub fn rename_over_open_file() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/y", b"new")?;
    let old = open("/data/x", O_CREAT | O_RDWR);
    check!(write(old, b"old") == 3, "write failed");
    check!(
        rename("/data/y", "/data/x") == 0,
        "rename over an open file failed"
    );
    check!(
        slurp("/data/x")? == b"new",
        "the name does not lead to the new file"
    );
    check!(
        pread(old, 10, 0) == Ok(b"old".to_vec()),
        "the replaced file's data changed"
    );
    check!(
        open("/data/y", O_RDONLY) == errno(ENOENT),
        "the source name survived"
    );
    check!(
        parked(&data_names()?) == 1,
        "the replaced file was not parked"
    );
    close(old);
    check!(
        parked(&data_names()?) == 0,
        "the replaced file outlived its last close"
    );

    // A failed rename must not leave the target displaced.
    let held = open("/data/x", O_RDWR);
    check!(
        rename("/data/missing", "/data/x") == errno(ENOENT),
        "rename of a missing file"
    );
    check!(
        parked(&data_names()?) == 0,
        "a failed rename left the target parked"
    );
    check!(
        write(held, b"!") == 1,
        "the descriptor lost its file after a failed rename"
    );
    check!(
        slurp("/data/x")? == b"!ew",
        "the target did not survive a failed rename"
    );
    check!(
        rename("/data/x", "/data/x") == 0,
        "a rename onto itself failed"
    );
    check!(
        write(held, b"?") == 1,
        "the descriptor lost its file after a self-rename"
    );
    check!(
        slurp("/data/x")? == b"!?w",
        "a self-rename displaced the file"
    );
    close(held);
    check!(path_call(SYS_UNLINK, "/data/x", 0) == 0, "unlink failed");
    data.check_clean()
}

/// Access is decided at `open`: a denied open is `EACCES`, and a descriptor
/// opened while privileged stays usable after the process drops privilege.
pub fn permission_denials() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open_mode("/data/secret", O_CREAT | O_RDWR, 0o600);
    check!(write(fd, b"top secret") == 10, "write failed");
    put("/data/readable", b"open")?;

    credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
    check!(
        open("/data/secret", O_RDONLY) == errno(EACCES),
        "a stranger read a 0600 file"
    );
    check!(
        open("/data/secret", O_WRONLY) == errno(EACCES),
        "a stranger wrote a 0600 file"
    );
    check!(
        open("/data/readable", O_WRONLY) == errno(EACCES),
        "a stranger wrote a 0644 file"
    );
    check!(
        open("/data/new", O_CREAT | O_WRONLY) == errno(EACCES),
        "a stranger created a file in a root directory"
    );
    check!(
        path_call(SYS_UNLINK, "/data/readable", 0) == errno(EACCES),
        "a stranger unlinked a file"
    );
    check!(
        path_call(SYS_TRUNCATE, "/data/secret", 0) == errno(EACCES),
        "a stranger truncated a file"
    );
    let reader = open("/data/readable", O_RDONLY);
    check!(
        read(reader, 10) == Ok(b"open".to_vec()),
        "a stranger could not read a 0644 file"
    );
    check!(
        write(reader, b"x") == errno(EBADF),
        "a write on a read-only open"
    );
    close(reader);

    // The descriptor opened as root is unaffected by the credential change.
    check!(
        pread(fd, 20, 0) == Ok(b"top secret".to_vec()),
        "an open descriptor was revoked"
    );
    check!(
        write(fd, b"!") == 1,
        "an open descriptor lost its write access"
    );
    credentials::set(task::current(), Cred::ROOT);
    close(fd);
    check!(
        slurp("/data/secret")? == b"top secret!",
        "the write through the kept fd differs"
    );
    data.check_clean()
}

/// The process that creates a file may open it whatever mode it gave the new
/// file: `O_CREAT | O_RDWR` with 0o200 must not fail the read check. Only that
/// creating open is exempt; a later read open is still denied.
pub fn create_with_no_read_bit() -> Result<(), String> {
    let data = Data::new(0)?;
    let umask = crate::fs::abi_set_umask(0);
    let made = path_call(SYS_MKDIR, "/data/drop", 0o777);
    crate::fs::abi_set_umask(umask);
    check!(made == 0, "mkdir failed");

    credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
    let fd = open_mode("/data/drop/w", O_CREAT | O_RDWR, 0o200);
    let created_ok = write(fd, b"x") == 1;
    let reopen = open("/data/drop/w", O_RDONLY);
    credentials::set(task::current(), Cred::ROOT);

    check!(created_ok, "the creating open of a 0o200 file was refused");
    check!(
        reopen == errno(EACCES),
        "a later read open of the write-only file was allowed"
    );
    close(fd);
    data.check_clean()
}

/// A volume whose device cannot be written still reads; every change is
/// `EROFS` and leaves nothing behind.
pub fn read_only_volume() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/ro", b"fixed")?;
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync failed");
    data.disk.set_read_only(true);
    data.remount()?;

    check!(
        slurp("/data/ro")? == b"fixed",
        "a read-only volume did not read"
    );
    let fd = open("/data/ro", O_RDWR);
    check!(fd < 16, "opening for write returned {fd:#x}");
    check!(
        write(fd, b"x") == errno(EROFS),
        "a write to a read-only volume"
    );
    check!(
        pwrite(fd, b"x", 0) == errno(EROFS),
        "a pwrite to a read-only volume"
    );
    check!(
        syscall(SYS_FTRUNCATE, fd, 0, 0, 0) == errno(EROFS),
        "ftruncate on a read-only volume"
    );
    check!(fstat_size(fd)? == 5, "a refused write changed the size");
    check!(
        syscall(SYS_FSYNC, fd, 0, 0, 0) == 0,
        "fsync on a read-only volume"
    );
    close(fd);
    check!(
        open("/data/new", O_CREAT | O_RDWR) == errno(EROFS),
        "creating on a read-only volume"
    );
    check!(
        path_call(SYS_UNLINK, "/data/ro", 0) == errno(EROFS),
        "unlinking on a read-only volume"
    );
    check!(
        rename("/data/ro", "/data/moved") == errno(EROFS),
        "renaming on a read-only volume"
    );
    check!(
        path_call(SYS_TRUNCATE, "/data/ro", 0) == errno(EROFS),
        "truncating on a read-only volume"
    );
    check!(
        slurp("/data/ro")? == b"fixed",
        "a refused change altered the file"
    );

    data.disk.set_read_only(false);
    data.remount()?;
    check!(
        path_call(SYS_UNLINK, "/data/ro", 0) == 0,
        "unlink failed once writable"
    );
    data.check_clean()
}
