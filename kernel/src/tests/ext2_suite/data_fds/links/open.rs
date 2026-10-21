//! An open file reached by several names: unlink and rename of one name, one
//! registry entry for all of them, one set of locks.

use super::*;

const SYS_RENAME_: u64 = 82;
const LOCK_EX: u64 = 2;
const LOCK_NB: u64 = 4;
const LOCK_UN: u64 = 8;

fn rename(from: &str, to: &str) -> u64 {
    let (from, to) = (cstr(from), cstr(to));
    syscall(SYS_RENAME_, from.as_ptr() as u64, to.as_ptr() as u64, 0, 0)
}

fn unlink(path: &str) -> u64 {
    path_call(SYS_UNLINK, path, 0)
}

/// Unlinking one name of an open file that has another parks nothing: the
/// name goes, the descriptor and the other name go on, and the count says 1.
/// Only when the last name goes is the file parked, with a count of 0, until
/// the last close.
pub fn unlink_while_open() -> Result<(), String> {
    let data = Data::new(0)?;
    for base in BASES {
        let parked_before = parked_in(base)?;
        let (a, b) = (format!("{base}/ua"), format!("{base}/ub"));
        put(&a, b"orphan data")?;
        let fd = open(&a, O_RDWR);
        check!(fd < 64, "open({a}) returned {fd:#x}");
        check!(link(&a, &b) == 0, "link");
        check!(unlink(&a) == 0, "{base}: unlink of an open linked name");
        check!(
            parked_in(base)? == parked_before,
            "{base}: a file with another name was parked"
        );
        check!(fd_links(fd)? == 1, "{base}: fstat after one name went");
        check!(ino_links(&b)?.1 == 1, "{base}: stat of the other name");
        check!(
            pread(fd, 6, 0) == Ok(b"orphan".to_vec()),
            "{base}: the descriptor lost its file"
        );
        check!(pwrite(fd, b"++", 11) == 2, "{base}: write after unlink");
        check!(slurp(&b)? == b"orphan data++", "{base}: the other name");
        // The last name: now the file is parked until the close.
        check!(unlink(&b) == 0, "{base}: unlink of the last name");
        check!(
            parked_in(base)? == parked_before + 1,
            "{base}: the last name of an open file was not parked"
        );
        check!(fd_links(fd)? == 0, "{base}: an unlinked open file's count");
        check!(
            pread(fd, 4, 0) == Ok(b"orph".to_vec()),
            "{base}: the parked file lost its data"
        );
        check!(close(fd) == 0, "close");
        check!(
            parked_in(base)? == parked_before,
            "{base}: the last close left the parked file"
        );
    }
    data.check_clean()
}

/// `rename` onto a second name of the same file is a no-op that keeps both;
/// a rename over one name of an open linked file drops only that name (no
/// parking), and the descriptor keeps its file through every step.
pub fn renames() -> Result<(), String> {
    let data = Data::new(0)?;
    for base in BASES {
        let parked_before = parked_in(base)?;
        let (a, b, c) = (
            format!("{base}/ra"),
            format!("{base}/rb"),
            format!("{base}/rc"),
        );
        put(&a, b"first")?;
        check!(link(&a, &b) == 0, "link");
        check!(rename(&a, &b) == 0, "{base}: rename onto a second name");
        check!(
            ino_links(&a)?.1 == 2 && ino_links(&b)?.1 == 2,
            "{base}: a rename between two names of a file removed one"
        );
        let fd = open(&b, O_RDWR);
        check!(fd < 64, "open({b}) returned {fd:#x}");
        put(&c, b"second")?;
        check!(
            rename(&c, &b) == 0,
            "{base}: rename over an open linked name"
        );
        check!(
            parked_in(base)? == parked_before,
            "{base}: a rename over a name with another left parked the file"
        );
        check!(slurp(&b)? == b"second", "{base}: the new file at {b}");
        check!(slurp(&a)? == b"first", "{base}: the other name");
        check!(fd_links(fd)? == 1, "{base}: count after the rename");
        check!(
            pread(fd, 5, 0) == Ok(b"first".to_vec()),
            "{base}: the descriptor changed files"
        );
        // The open file takes its old name back over the other file.
        check!(rename(&a, &b) == 0, "{base}: rename back");
        check!(open(&a, O_RDONLY) == errno(ENOENT), "{base}: {a} stayed");
        check!(pwrite(fd, b"F", 0) == 1, "{base}: write");
        check!(
            slurp(&b)? == b"First",
            "{base}: the descriptor's file at {b}"
        );
        check!(unlink(&b) == 0, "{base}: unlink of the last name");
        check!(
            parked_in(base)? == parked_before + 1,
            "{base}: the last name of an open file was not parked"
        );
        check!(close(fd) == 0, "close");
        check!(parked_in(base)? == parked_before, "{base}: left parked");
    }
    data.check_clean()
}

/// Two names of one file open as one registry entry, so one page cache: the
/// registry counts one file, and a write through one descriptor is in the
/// cache the other maps.
pub fn one_registry_entry() -> Result<(), String> {
    let data = Data::new(0)?;
    for base in BASES {
        let before = crate::fs::openfile::open_files();
        let (a, b) = (format!("{base}/oa"), format!("{base}/ob"));
        put(&a, b"same file")?;
        check!(link(&a, &b) == 0, "link");
        let (fa, fb) = (open(&a, O_RDWR), open(&b, O_RDONLY));
        check!(fa < 64 && fb < 64, "open gave {fa:#x}/{fb:#x}");
        check!(
            file_of(fa)?.inode_id() == file_of(fb)?.inode_id(),
            "{base}: two names opened two registry entries"
        );
        check!(
            crate::fs::openfile::open_files() == before + 1,
            "{base}: the registry counts {} files",
            crate::fs::openfile::open_files() - before
        );
        check!(pwrite(fa, b"SAME", 0) == 4, "pwrite");
        check!(
            pread(fb, 9, 0) == Ok(b"SAME file".to_vec()),
            "{base}: the other name's descriptor"
        );
        close(fa);
        close(fb);
        check!(unlink(&a) == 0 && unlink(&b) == 0, "{base}: unlink");
        check!(
            crate::fs::openfile::open_files() == before,
            "{base}: the entry outlived its descriptors"
        );
    }
    data.check_clean()
}

/// `flock` through one name holds against a description opened through the
/// other: locks belong to the file, not to the name.
pub fn locks_shared_by_names() -> Result<(), String> {
    let data = Data::new(0)?;
    for base in BASES {
        let (a, b) = (format!("{base}/la"), format!("{base}/lb"));
        put(&a, b"locked")?;
        check!(link(&a, &b) == 0, "link");
        let (fa, fb) = (open(&a, O_RDWR), open(&b, O_RDWR));
        check!(fa < 64 && fb < 64, "open gave {fa:#x}/{fb:#x}");
        check!(
            syscall(SYS_FLOCK, fa, LOCK_EX, 0, 0) == 0,
            "flock through {a}"
        );
        check!(
            syscall(SYS_FLOCK, fb, LOCK_EX | LOCK_NB, 0, 0) == errno(EAGAIN),
            "{base}: a lock through the other name was granted"
        );
        check!(syscall(SYS_FLOCK, fa, LOCK_UN, 0, 0) == 0, "unlock");
        check!(
            syscall(SYS_FLOCK, fb, LOCK_EX | LOCK_NB, 0, 0) == 0,
            "{base}: the lock is free again"
        );
        close(fa);
        close(fb);
        check!(unlink(&a) == 0 && unlink(&b) == 0, "{base}: unlink");
    }
    data.check_clean()
}

/// An entry left by a file deleted behind the registry's back (a native
/// unlink) shares its inode number with the file that reused it: the
/// registry must still find the new file open, so an unlink parks it rather
/// than freeing data a descriptor reads.
pub fn stale_entry_does_not_hide_the_open_file() -> Result<(), String> {
    use crate::fs::vfs::Filesystem;
    let data = Data::new(0)?;
    let parked_before = parked_in("/data")?;
    put("/data/st", b"first file")?;
    let stale = open("/data/st", O_RDWR);
    check!(stale < 64, "open returned {stale:#x}");
    let ino = ino_links("/data/st")?.0;
    data.fs.unlink("st").map_err(fs_error)?;
    data.fs.create("st", 0o644, Id::ROOT).map_err(fs_error)?;
    data.fs.write("st", 0, b"second").map_err(fs_error)?;
    check!(
        ino_links("/data/st")?.0 == ino,
        "the new file did not reuse the inode number, the case is not set up"
    );
    let live = open("/data/st", O_RDONLY);
    check!(live < 64, "open returned {live:#x}");
    check!(unlink("/data/st") == 0, "unlink of the new file");
    check!(
        parked_in("/data")? == parked_before + 1,
        "the open new file was not parked: the stale entry hid it"
    );
    check!(
        pread(live, 6, 0) == Ok(b"second".to_vec()),
        "the new file's descriptor lost its data"
    );
    check!(
        pread(stale, 6, 0) == Err(errno(ENOENT)),
        "the deleted file's descriptor reads"
    );
    close(live);
    close(stale);
    check!(parked_in("/data")? == parked_before, "left parked");
    data.check_clean()
}
