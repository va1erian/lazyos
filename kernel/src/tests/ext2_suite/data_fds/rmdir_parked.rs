//! `rmdir` of a directory whose only entries are files unlinked while still
//! open (issue #612): on Linux the names are gone, so the directory is empty.
//! Run on the ext2 volume (`/data`) and on the ramfs `/tmp`, which keep an
//! open file differently (by inode and by path).

use super::*;

const SYS_RMDIR: u64 = 84;
const ENOTEMPTY: u64 = 39;

/// The mounts under test: ext2 and ramfs.
const MOUNTS: [&str; 2] = ["/data", "/tmp"];

/// Hidden `.unlinked-*` entries directly in `dir`.
fn parked_in(dir: &str) -> Result<usize, String> {
    let entries = crate::fs::abi_readdir(Id::ROOT, dir).map_err(fs_error)?;
    Ok(entries
        .iter()
        .filter(|entry| entry.name.starts_with(".unlinked-"))
        .count())
}

fn rmdir(path: &str) -> u64 {
    path_call(SYS_RMDIR, path, 0)
}

/// `open; unlink; rmdir` succeeds, the directory is gone, the descriptor keeps
/// reading and writing its file, and the last close frees it.
pub fn rmdir_with_open_unlinked_file() -> Result<(), String> {
    let data = Data::new(0)?;
    let baseline = free_space()?;
    for mount in MOUNTS {
        let dir = format!("{mount}/d");
        let file = format!("{dir}/f");
        check!(path_call(SYS_MKDIR, &dir, 0o755) == 0, "mkdir({dir})");
        let fd = open(&file, O_CREAT | O_RDWR);
        check!(fd < 16, "open({file}) returned {fd:#x}");
        check!(write(fd, b"still here") == 10, "write to {file}");
        check!(path_call(SYS_UNLINK, &file, 0) == 0, "unlink({file})");
        let ret = rmdir(&dir);
        check!(
            ret == 0,
            "rmdir({dir}) with a parked file returned {ret:#x}"
        );
        check!(
            open(&dir, O_RDONLY) == errno(ENOENT),
            "{dir} survived its rmdir"
        );
        check!(
            pread(fd, 10, 0) == Ok(b"still here".to_vec()),
            "{file}'s data went with its directory"
        );
        check!(
            pwrite(fd, b"!", 10) == 1,
            "write after the rmdir on {mount}"
        );
        check!(fstat_size(fd)? == 11, "fstat after the rmdir on {mount}");
        check!(
            parked_in(mount)? == 1,
            "{mount} does not hold the parked file"
        );
        close(fd);
        check!(
            parked_in(mount)? == 0,
            "the parked file outlived its last close on {mount}"
        );
        // The name is free for a new directory.
        check!(path_call(SYS_MKDIR, &dir, 0o755) == 0, "mkdir({dir}) again");
        check!(rmdir(&dir) == 0, "rmdir of the new {dir}");
    }
    check!(
        free_space()? == baseline,
        "the parked file's blocks or inodes were not returned"
    );
    data.check_clean()
}

/// Nested directories empty one level at a time; a directory that also holds
/// a real name stays `ENOTEMPTY` and keeps its parked file where it was.
pub fn rmdir_parked_nested_and_not_empty() -> Result<(), String> {
    let data = Data::new(0)?;
    for mount in MOUNTS {
        let (outer, inner) = (format!("{mount}/a"), format!("{mount}/a/b"));
        check!(path_call(SYS_MKDIR, &outer, 0o755) == 0, "mkdir({outer})");
        check!(path_call(SYS_MKDIR, &inner, 0o755) == 0, "mkdir({inner})");
        let deep = open(&format!("{inner}/f"), O_CREAT | O_RDWR);
        let shallow = open(&format!("{outer}/g"), O_CREAT | O_RDWR);
        check!(write(deep, b"deep") == 4, "write deep");
        check!(write(shallow, b"shallow") == 7, "write shallow");
        put(&format!("{outer}/keep"), b"kept")?;
        check!(
            path_call(SYS_UNLINK, &format!("{inner}/f"), 0) == 0,
            "unlink deep"
        );
        check!(
            path_call(SYS_UNLINK, &format!("{outer}/g"), 0) == 0,
            "unlink shallow"
        );

        check!(rmdir(&inner) == 0, "rmdir({inner})");
        check!(
            rmdir(&outer) == errno(ENOTEMPTY),
            "rmdir({outer}) with a real entry left"
        );
        check!(
            parked_in(&outer)? == 1,
            "a refused rmdir moved {outer}'s parked file"
        );
        check!(
            pread(shallow, 7, 0) == Ok(b"shallow".to_vec()),
            "a refused rmdir lost the parked file"
        );
        check!(
            slurp(&format!("{outer}/keep"))? == b"kept",
            "the real file changed"
        );
        check!(
            path_call(SYS_UNLINK, &format!("{outer}/keep"), 0) == 0,
            "unlink keep"
        );
        check!(
            rmdir(&outer) == 0,
            "rmdir({outer}) once only parked files remain"
        );
        check!(pread(deep, 4, 0) == Ok(b"deep".to_vec()), "deep file lost");
        check!(parked_in(mount)? == 2, "both parked files moved to {mount}");
        close(deep);
        close(shallow);
        check!(parked_in(mount)? == 0, "parked files outlived their close");
    }
    data.check_clean()
}

/// A caller who may not remove the directory gets `EACCES`, and the parked
/// file stays exactly where it was.
pub fn rmdir_parked_refused_changes_nothing() -> Result<(), String> {
    let data = Data::new(0)?;
    for mount in MOUNTS {
        let parent = format!("{mount}/p");
        let dir = format!("{parent}/d");
        check!(path_call(SYS_MKDIR, &parent, 0o755) == 0, "mkdir({parent})");
        check!(path_call(SYS_MKDIR, &dir, 0o777) == 0, "mkdir({dir})");
        let file = format!("{dir}/f");
        let fd = open(&file, O_CREAT | O_RDWR);
        check!(write(fd, b"x") == 1, "write");
        check!(path_call(SYS_UNLINK, &file, 0) == 0, "unlink({file})");
        credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
        let ret = rmdir(&dir);
        credentials::set(task::current(), Cred::ROOT);
        check!(
            ret == errno(EACCES),
            "unprivileged rmdir({dir}) returned {ret:#x}"
        );
        check!(
            parked_in(&dir)? == 1,
            "a refused rmdir moved the parked file"
        );
        check!(
            parked_in(mount)? == 0,
            "a refused rmdir left a name at {mount}"
        );
        check!(pread(fd, 1, 0) == Ok(b"x".to_vec()), "parked file lost");
        close(fd);
        check!(rmdir(&dir) == 0, "rmdir({dir}) after the close");
        check!(rmdir(&parent) == 0, "rmdir({parent})");
    }
    data.check_clean()
}

/// Many rounds of `mkdir; open; unlink; rmdir` with descriptors held across
/// rounds: nothing leaks (descriptors, registered files, blocks, inodes) and
/// the volume stays consistent.
pub fn soak_rmdir_parked() -> Result<(), String> {
    const ROUNDS: usize = 300;
    const HELD: usize = 6;
    let data = Data::new(0)?;
    let baseline = free_space()?;
    for mount in MOUNTS {
        let mut held: Vec<u64> = Vec::new();
        for round in 0..ROUNDS {
            let dir = format!("{mount}/s{}", round % 7);
            let file = format!("{dir}/f");
            check!(
                path_call(SYS_MKDIR, &dir, 0o755) == 0,
                "round {round}: mkdir"
            );
            let fd = open(&file, O_CREAT | O_RDWR);
            check!(fd < 64, "round {round}: open returned {fd:#x}");
            check!(
                write(fd, &[round as u8; 300]) == 300,
                "round {round}: write"
            );
            check!(
                path_call(SYS_UNLINK, &file, 0) == 0,
                "round {round}: unlink"
            );
            check!(rmdir(&dir) == 0, "round {round}: rmdir");
            held.push(fd);
            if held.len() > HELD {
                let old = held.remove(0);
                check!(
                    pread(old, 1, 299).map(|b| b.len()) == Ok(1),
                    "round {round}: an older parked file lost its data"
                );
                close(old);
            }
        }
        for fd in held {
            close(fd);
        }
        check!(parked_in(mount)? == 0, "parked files leaked on {mount}");
    }
    check!(
        free_space()? == baseline,
        "the soak leaked blocks or inodes"
    );
    data.check_clean()
}
