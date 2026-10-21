//! `link`/`linkat` on names: what a link makes, what is refused, who may,
//! and that it lasts.

use super::*;
use crate::fs::vfs::Filesystem;

/// A second name is the same file: the same inode, a link count of two on
/// both names, a write through one read through the other; unlinking one
/// leaves the other with the data and a count of one.
pub fn names_share_a_file() -> Result<(), String> {
    let data = Data::new(0)?;
    for base in BASES {
        let (a, b) = (format!("{base}/a"), format!("{base}/b"));
        put(&a, b"hello")?;
        check!(ino_links(&a)?.1 == 1, "{a}: a new file has one name");
        check!(link(&a, &b) == 0, "link({a}, {b})");
        let (ino_a, links_a) = ino_links(&a)?;
        let (ino_b, links_b) = ino_links(&b)?;
        check!(ino_a == ino_b, "{base}: two inodes for one file");
        check!(
            links_a == 2 && links_b == 2,
            "{base}: link counts {links_a}/{links_b}"
        );
        let fd = open(&b, O_RDWR);
        check!(fd < 64, "open({b}) returned {fd:#x}");
        check!(fd_links(fd)? == 2, "{base}: fstat after link");
        check!(pwrite(fd, b"HE", 0) == 2, "{base}: pwrite through {b}");
        check!(close(fd) == 0, "close");
        check!(slurp(&a)? == b"HEllo", "{base}: {a} misses the write");
        check!(path_call(SYS_UNLINK, &a, 0) == 0, "unlink({a})");
        check!(
            open(&a, O_RDONLY) == errno(ENOENT),
            "{base}: the unlinked name opens"
        );
        check!(ino_links(&b)? == (ino_b, 1), "{base}: count after unlink");
        check!(slurp(&b)? == b"HEllo", "{base}: {b} lost the data");
        // A link with `linkat` and `AT_SYMLINK_FOLLOW`, relative to a
        // directory descriptor.
        let dir = format!("{base}/d");
        check!(path_call(SYS_MKDIR, &dir, 0o755) == 0, "mkdir {dir}");
        let dfd = open(&dir, O_RDONLY | O_DIRECTORY);
        check!(dfd < 64, "open({dir}) returned {dfd:#x}");
        check!(
            linkat(AT_FDCWD, &b, dfd, "c", AT_SYMLINK_FOLLOW) == 0,
            "{base}: linkat into a directory descriptor"
        );
        check!(
            linkat(dfd, "c", AT_FDCWD, &a, 0) == 0,
            "{base}: linkat from a directory descriptor"
        );
        check!(close(dfd) == 0, "close dir");
        check!(ino_links(&a)? == (ino_b, 3), "{base}: three names");
        for name in [a, b, format!("{dir}/c")] {
            check!(path_call(SYS_UNLINK, &name, 0) == 0, "unlink({name})");
        }
        check!(path_call(SYS_RMDIR, &dir, 0) == 0, "rmdir {dir}");
    }
    data.check_clean()
}

/// What `link` refuses, and with which errno.
pub fn refusals() -> Result<(), String> {
    let data = Data::new(0)?;
    for base in BASES {
        let (f, d) = (format!("{base}/f"), format!("{base}/d"));
        put(&f, b"f")?;
        check!(path_call(SYS_MKDIR, &d, 0o755) == 0, "mkdir {d}");
        let cases: [(&str, String, String, u64); 7] = [
            ("onto itself", f.clone(), f.clone(), EEXIST),
            ("onto a directory", f.clone(), d.clone(), EEXIST),
            ("a directory", d.clone(), format!("{base}/d2"), EPERM),
            (
                "a missing file",
                format!("{base}/nope"),
                format!("{base}/x"),
                ENOENT,
            ),
            (
                "into a missing directory",
                f.clone(),
                format!("{base}/no/x"),
                ENOENT,
            ),
            (
                "onto a reserved name",
                f.clone(),
                format!("{base}/.unlinked-9"),
                EINVAL,
            ),
            ("onto the root", f.clone(), String::from("/"), EEXIST),
        ];
        for (what, old, new, want) in cases {
            let got = link(&old, &new);
            check!(got == errno(want), "{base}: link {what} gave {got:#x}");
        }
        check!(
            linkat(AT_FDCWD, &f, AT_FDCWD, &format!("{base}/x"), 0x1) == errno(EINVAL),
            "{base}: an unknown linkat flag"
        );
        check!(
            linkat(AT_FDCWD, "", AT_FDCWD, &format!("{base}/x"), AT_EMPTY_PATH) == errno(ENOENT),
            "{base}: linkat AT_EMPTY_PATH"
        );
        check!(
            linkat(AT_FDCWD, "", AT_FDCWD, &format!("{base}/x"), 0) == errno(ENOENT),
            "{base}: linkat of an empty path"
        );
        // Symbolic links are still not stored anywhere.
        let (target, name) = (cstr(&f), cstr(&format!("{base}/sym")));
        check!(
            syscall(
                SYS_SYMLINK,
                target.as_ptr() as u64,
                name.as_ptr() as u64,
                0,
                0
            ) == errno(EPERM),
            "{base}: symlink"
        );
        check!(ino_links(&f)?.1 == 1, "{base}: a refused link counted");
        check!(path_call(SYS_UNLINK, &f, 0) == 0, "unlink {f}");
        check!(path_call(SYS_RMDIR, &d, 0) == 0, "rmdir {d}");
    }
    // Two mounts: EXDEV, whichever way.
    put("/data/m", b"m")?;
    put("/tmp/m", b"m")?;
    check!(link("/data/m", "/tmp/m2") == errno(EXDEV), "ext2 to ramfs");
    check!(link("/tmp/m", "/data/m2") == errno(EXDEV), "ramfs to ext2");
    // A parked orphan gets no name back, even made behind the ABI's back.
    data.fs
        .create(".unlinked-77", 0o644, Id::ROOT)
        .map_err(fs_error)?;
    check!(
        link("/data/.unlinked-77", "/data/revived") == errno(ENOENT),
        "a reserved name was linked"
    );
    data.fs.unlink(".unlinked-77").map_err(fs_error)?;
    for path in ["/data/m", "/tmp/m"] {
        check!(path_call(SYS_UNLINK, path, 0) == 0, "unlink {path}");
    }
    data.check_clean()
}

/// `fs.protected_hardlinks`: a user who does not own a file may link it only
/// when they may read and write it and it is not set-user-ID (or
/// set-group-ID and group-executable). The owner and root always may.
pub fn protected_hardlinks() -> Result<(), String> {
    let data = Data::new(0)?;
    for base in BASES {
        let shared = format!("{base}/shared");
        check!(path_call(SYS_MKDIR, &shared, 0o777) == 0, "mkdir {shared}");
        check!(path_call(SYS_CHMOD, &shared, 0o777) == 0, "chmod {shared}");
        credentials::set(task::current(), attrs::ALICE);
        let files = [
            ("private", 0o644u64, EPERM),
            ("open", 0o666, 0),
            ("setuid", 0o4777, EPERM),
            ("setgid", 0o2777, EPERM),
            ("setgid-noexec", 0o2666, 0),
        ];
        for (name, mode, _) in files {
            let path = format!("{shared}/{name}");
            let fd = open_mode(&path, O_CREAT | O_WRONLY, mode);
            check!(fd < 64, "create {path} returned {fd:#x}");
            close(fd);
            // The umask dropped the permission bits; chmod puts them back.
            check!(path_call(SYS_CHMOD, &path, mode) == 0, "chmod {path}");
        }
        // The owner links any of them.
        check!(
            link(&format!("{shared}/private"), &format!("{shared}/mine")) == 0,
            "{base}: the owner could not link"
        );
        credentials::set(task::current(), attrs::BOB);
        for (name, _, want) in files {
            let got = link(&format!("{shared}/{name}"), &format!("{shared}/bob-{name}"));
            check!(
                got == errno(want),
                "{base}: bob linking {name} gave {got:#x}"
            );
        }
        credentials::set(task::current(), Cred::ROOT);
        check!(
            link(&format!("{shared}/setuid"), &format!("{shared}/root")) == 0,
            "{base}: root could not link"
        );
        for entry in crate::fs::abi_readdir(Id::ROOT, &shared).map_err(fs_error)? {
            let path = format!("{shared}/{}", entry.name);
            check!(path_call(SYS_UNLINK, &path, 0) == 0, "unlink {path}");
        }
        check!(path_call(SYS_RMDIR, &shared, 0) == 0, "rmdir {shared}");
    }
    data.check_clean()
}

/// A read-only volume refuses a link with `EROFS` and keeps its count.
pub fn read_only_volume() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/ro", b"fixed")?;
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync failed");
    data.disk.set_read_only(true);
    data.remount()?;
    check!(
        link("/data/ro", "/data/ro2") == errno(EROFS),
        "a link on a read-only volume"
    );
    check!(ino_links("/data/ro")?.1 == 1, "the count moved");
    data.disk.set_read_only(false);
    data.remount()?;
    data.check_clean()
}

/// Links reach the disk: after a remount both names are there with the
/// count, and the checker finds the volume consistent.
pub fn survives_remount() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/one", b"persisted")?;
    check!(path_call(SYS_MKDIR, "/data/sub", 0o755) == 0, "mkdir");
    check!(link("/data/one", "/data/sub/two") == 0, "link");
    check!(link("/data/one", "/data/three") == 0, "link");
    check!(path_call(SYS_UNLINK, "/data/three", 0) == 0, "unlink");
    check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync failed");
    data.remount()?;
    let (ino, links) = ino_links("/data/sub/two")?;
    check!(
        ino_links("/data/one")? == (ino, 2) && links == 2,
        "after a remount: {links} links"
    );
    check!(slurp("/data/sub/two")? == b"persisted", "the data");
    check_volume(data.disk, VOLUME_BLOCKS)?;
    for path in ["/data/one", "/data/sub/two"] {
        check!(path_call(SYS_UNLINK, path, 0) == 0, "unlink {path}");
    }
    check!(path_call(SYS_RMDIR, "/data/sub", 0) == 0, "rmdir");
    data.check_clean()
}
