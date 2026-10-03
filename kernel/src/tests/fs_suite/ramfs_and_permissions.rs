//! Path resolution and mounts, ramfs create/write/read/rename/
//! unlink, and the owner/group/other permission matrix.

use super::*;

/// `Path` folds `.`/`..` lexically (clamped at the root), collapses
/// slashes, and roots relative inputs; mounts resolve by longest prefix,
/// and `..` folds before mount lookup.
pub fn path_resolution_and_mounts() -> Result<(), String> {
    for (raw, expected) in [
        ("/", "/"),
        (".", "/"),
        ("/..", "/"),
        ("/a/../..", "/"),
        ("/a/./b/../c", "/a/c"),
        ("//a//b/", "/a/b"),
        ("a/b", "/a/b"),
    ] {
        let folded = Path::parse(raw).to_path_string();
        check!(
            folded == expected,
            "{raw:?} folded to {folded:?}, expected {expected:?}"
        );
    }
    check!(
        Path::parse("/a").is_absolute() && !Path::parse("a").is_absolute(),
        "absolute detection is wrong"
    );
    check!(
        Path::parse("/a/b").name() == Some("b") && Path::parse("/").name().is_none(),
        "the final component is wrong"
    );

    let root = Id::ROOT;
    let mut vfs = Vfs::new();
    vfs.mount(
        "/",
        Arc::new(RamFs::new()),
        crate::fs::vfs::MountFlags::default(),
    )
    .map_err(fs_error)?;
    vfs.mount(
        "/tmp",
        Arc::new(RamFs::new()),
        crate::fs::vfs::MountFlags::default(),
    )
    .map_err(fs_error)?;

    // A file written under /tmp lands in the /tmp filesystem, not the root.
    vfs.create(root, "/tmp/scratch.txt", 0o644)
        .map_err(fs_error)?;
    vfs.create(root, "/hello.txt", 0o644).map_err(fs_error)?;
    check!(
        vfs.stat(root, "/tmp/scratch.txt").is_ok(),
        "/tmp/scratch.txt is missing"
    );
    check!(
        vfs.stat(root, "/hello.txt").is_ok(),
        "/hello.txt is missing"
    );
    check!(
        vfs.stat(root, "/scratch.txt").err() == Some(FsError::NotFound),
        "/scratch.txt leaked out of the /tmp mount"
    );

    // The longest mount point wins: /tmp/nested is its own filesystem.
    vfs.mount(
        "/tmp/nested",
        Arc::new(RamFs::new()),
        crate::fs::vfs::MountFlags::default(),
    )
    .map_err(fs_error)?;
    vfs.create(root, "/tmp/nested/inner.txt", 0o644)
        .map_err(fs_error)?;
    check!(
        vfs.stat(root, "/tmp/nested/inner.txt").is_ok(),
        "the deepest mount did not receive the file"
    );
    check!(
        vfs.stat(root, "/tmp/inner.txt").err() == Some(FsError::NotFound),
        "the nested mount leaked into /tmp"
    );

    // `..` folds before mount resolution: /tmp/../hello.txt is the root fs.
    check!(
        vfs.stat(root, "/tmp/../hello.txt").is_ok(),
        ".. did not fold to the root"
    );
    check!(
        vfs.stat(root, "/tmp/../scratch.txt").err() == Some(FsError::NotFound),
        ".. resolved inside the /tmp mount"
    );

    let mounts = vfs.mounts();
    check!(
        mounts
            .iter()
            .any(|(point, name)| point == "/tmp" && *name == "ramfs"),
        "mount table is {mounts:?}"
    );
    Ok(())
}

/// ramfs: create/write (at offsets), read, stat, readdir, rename, unlink,
/// error cases, and the umask applied at creation.
pub fn ramfs_create_write_read_rename_unlink() -> Result<(), String> {
    let root = Id::ROOT;
    let mut vfs = ram_vfs();

    vfs.mkdir(root, "/docs", 0o755).map_err(fs_error)?;
    vfs.create(root, "/docs/note.txt", 0o644)
        .map_err(fs_error)?;
    let meta = vfs.stat(root, "/docs/note.txt").map_err(fs_error)?;
    check!(
        meta.kind == FileKind::File && meta.size == 0 && meta.mode & vfs::S_IFMT == vfs::S_IFREG,
        "fresh file metadata is {meta:?}"
    );

    check!(
        vfs.write(root, "/docs/note.txt", 0, b"hello")
            .map_err(fs_error)?
            == 5,
        "first write was short"
    );
    vfs.write(root, "/docs/note.txt", 5, b" world")
        .map_err(fs_error)?;
    let data = vfs.read_file(root, "/docs/note.txt").map_err(fs_error)?;
    check!(data == b"hello world".to_vec(), "contents are {data:?}");
    check!(
        vfs.stat(root, "/docs/note.txt").map_err(fs_error)?.size == 11,
        "stat did not see the appended bytes"
    );

    let mut buf = [0u8; 4];
    let read = vfs
        .read(root, "/docs/note.txt", 6, &mut buf)
        .map_err(fs_error)?;
    check!(
        read == 4 && &buf == b"worl",
        "offset read got {read} bytes {buf:?}"
    );
    check!(
        vfs.read(root, "/docs/note.txt", 99, &mut buf)
            .map_err(fs_error)?
            == 0,
        "read past EOF did not return 0"
    );

    let names: Vec<String> = vfs
        .readdir(root, "/docs")
        .map_err(fs_error)?
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    check!(names == ["note.txt"], "readdir is {names:?}");

    vfs.rename(root, "/docs/note.txt", "/docs/memo.txt")
        .map_err(fs_error)?;
    check!(
        vfs.stat(root, "/docs/note.txt").err() == Some(FsError::NotFound),
        "rename left the source behind"
    );
    check!(
        vfs.read_file(root, "/docs/memo.txt").map_err(fs_error)? == b"hello world".to_vec(),
        "rename lost the contents"
    );

    vfs.unlink(root, "/docs/memo.txt").map_err(fs_error)?;
    check!(
        vfs.stat(root, "/docs/memo.txt").err() == Some(FsError::NotFound),
        "unlink left the file behind"
    );
    check!(
        vfs.readdir(root, "/docs").map_err(fs_error)?.is_empty(),
        "readdir still lists the unlinked file"
    );

    check!(
        vfs.unlink(root, "/nope").err() == Some(FsError::NotFound),
        "unlink found a ghost"
    );
    check!(
        vfs.mkdir(root, "/docs", 0o755).err() == Some(FsError::Exists),
        "mkdir overwrote a dir"
    );
    check!(
        vfs.create(root, "/missing/file", 0o644).err() == Some(FsError::NotFound),
        "create succeeded in a missing directory"
    );
    check!(
        vfs.unlink(root, "/docs").err() == Some(FsError::IsDir),
        "unlink removed a directory"
    );
    check!(
        vfs.write(root, "/docs", 0, b"x").err() == Some(FsError::IsDir),
        "write succeeded on a directory"
    );

    // The umask masks creation modes (`umask(2)` semantics).
    let previous = vfs.set_umask(0o077);
    check!(
        previous == 0o022,
        "default umask is {previous:o}, expected 022"
    );
    vfs.create(root, "/private.txt", 0o666).map_err(fs_error)?;
    let meta = vfs.stat(root, "/private.txt").map_err(fs_error)?;
    check!(
        meta.mode & 0o777 == 0o600,
        "umask left mode {:o}, expected 600",
        meta.mode & 0o777
    );
    check!(vfs.umask() == 0o077, "umask readback is {:o}", vfs.umask());
    Ok(())
}

/// The owner/group/other matrix against kernel-stamped ids, root bypass,
/// `F_OK`, and directory traversal through a real VFS.
pub fn permission_matrix_owner_group_other() -> Result<(), String> {
    let file = Meta {
        ino: 5,
        mode: vfs::S_IFREG | 0o640,
        uid: 1000,
        gid: 100,
        size: 0,
        kind: FileKind::File,
        times: vfs::Times::default(),
    };
    let owner = Id::new(1000, 200);
    let group = Id::new(2000, 100);
    let other = Id::new(2000, 200);

    check!(
        vfs::check_access(&file, owner, vfs::READ | vfs::WRITE).is_ok(),
        "the owner was denied rw"
    );
    check!(
        vfs::check_access(&file, owner, vfs::EXECUTE).err() == Some(FsError::Access),
        "the owner was allowed x"
    );
    check!(
        vfs::check_access(&file, group, vfs::READ).is_ok(),
        "the group was denied r"
    );
    check!(
        vfs::check_access(&file, group, vfs::WRITE).err() == Some(FsError::Access),
        "the group was allowed w"
    );
    check!(
        vfs::check_access(&file, other, vfs::READ).err() == Some(FsError::Access),
        "other was allowed r"
    );
    check!(
        vfs::check_access(&file, Id::ROOT, vfs::READ | vfs::WRITE).is_ok(),
        "root did not bypass the mode bits"
    );
    // Executing a regular file needs one `x` bit even for root (Linux).
    check!(
        vfs::check_access(&file, Id::ROOT, vfs::EXECUTE).err() == Some(FsError::Access),
        "root could execute a file with no x bit"
    );
    let other_x = Meta {
        mode: vfs::S_IFREG | 0o641,
        ..file
    };
    check!(
        vfs::check_access(&other_x, Id::ROOT, vfs::EXECUTE).is_ok(),
        "root was denied x on a file others may execute"
    );
    check!(
        vfs::check_access(&file, other, 0).is_ok(),
        "an F_OK-style check failed"
    );

    // Through a VFS: a 0700 directory hides its contents from everyone but
    // its owner (and root), even when the file inside is world-readable.
    let mut vfs = ram_vfs();
    vfs.mkdir(Id::ROOT, "/home", 0o700).map_err(fs_error)?;
    vfs.create(Id::ROOT, "/home/secret", 0o644)
        .map_err(fs_error)?;
    check!(
        vfs.stat(Id::ROOT, "/home/secret").is_ok(),
        "root could not stat inside /home"
    );
    check!(
        vfs.stat(group, "/home/secret").err() == Some(FsError::Access),
        "the group traversed a 0700 directory"
    );
    check!(
        vfs.stat(other, "/home/secret").err() == Some(FsError::Access),
        "other traversed a 0700 directory"
    );

    // A world-readable file on a traversable path: read allowed, write not.
    vfs.mkdir(Id::ROOT, "/public", 0o755).map_err(fs_error)?;
    vfs.create(Id::ROOT, "/public/readme", 0o644)
        .map_err(fs_error)?;
    check!(
        vfs.read_file(other, "/public/readme").is_ok(),
        "the world could not read a 0644 file"
    );
    check!(
        vfs.write(other, "/public/readme", 0, b"x").err() == Some(FsError::Access),
        "the world could write a 0644 file"
    );
    Ok(())
}
