//! Traversal and the sticky bit, cache invalidation, the read-only
//! FAT volume, and `getdents64` over the Linux fd layer.

use super::*;

/// The sticky bit on shared directories: entry owner, directory owner, or
/// root may unlink/rename; others cannot. Also the pure rule function.
pub fn traversal_and_sticky_bits() -> Result<(), String> {
    let dir = Meta {
        ino: 6,
        mode: vfs::S_IFDIR | 0o1777,
        uid: 3000,
        gid: 300,
        size: 0,
        kind: FileKind::Dir,
        times: vfs::Times::default(),
    };
    let entry = Meta {
        ino: 7,
        mode: vfs::S_IFREG | 0o644,
        uid: 1000,
        gid: 100,
        size: 0,
        kind: FileKind::File,
        times: vfs::Times::default(),
    };
    check!(
        vfs::check_sticky(&dir, &entry, Id::new(3000, 1)).is_ok(),
        "the directory owner was denied"
    );
    check!(
        vfs::check_sticky(&dir, &entry, Id::new(1000, 1)).is_ok(),
        "the entry owner was denied"
    );
    check!(
        vfs::check_sticky(&dir, &entry, Id::new(2000, 1)).err() == Some(FsError::Access),
        "a stranger was allowed"
    );
    check!(
        vfs::check_sticky(&dir, &entry, Id::ROOT).is_ok(),
        "root was denied"
    );
    let normal = Meta {
        mode: vfs::S_IFDIR | 0o777,
        ..dir
    };
    check!(
        vfs::check_sticky(&normal, &entry, Id::new(2000, 1)).is_ok(),
        "a non-sticky directory restricted unlink"
    );

    // End to end: alice and bob share a sticky directory.
    let mut vfs = ram_vfs();
    // The default umask would clear the shared directory's world-write bit.
    vfs.set_umask(0);
    let alice = Id::new(1000, 100);
    let bob = Id::new(2000, 200);
    vfs.mkdir(Id::ROOT, "/shared", 0o1777).map_err(fs_error)?;
    vfs.create(alice, "/shared/alice.txt", 0o644)
        .map_err(fs_error)?;
    check!(
        vfs.unlink(bob, "/shared/alice.txt").err() == Some(FsError::Access),
        "bob removed alice's sticky entry"
    );
    check!(
        vfs.rename(bob, "/shared/alice.txt", "/shared/stolen.txt")
            .err()
            == Some(FsError::Access),
        "bob renamed alice's sticky entry"
    );
    check!(
        vfs.unlink(alice, "/shared/alice.txt").is_ok(),
        "alice could not remove her own entry"
    );
    vfs.create(alice, "/shared/alice2.txt", 0o644)
        .map_err(fs_error)?;
    check!(
        vfs.unlink(Id::ROOT, "/shared/alice2.txt").is_ok(),
        "root was sticky-blocked"
    );

    // A sticky directory owned by alice: she may remove bob's entry.
    vfs.mkdir(alice, "/shared/alice-dir", 0o1777)
        .map_err(fs_error)?;
    vfs.create(bob, "/shared/alice-dir/bob.txt", 0o644)
        .map_err(fs_error)?;
    check!(
        vfs.unlink(alice, "/shared/alice-dir/bob.txt").is_ok(),
        "the sticky directory owner could not remove an entry"
    );
    Ok(())
}

/// Caches serve repeated lookups, mutations refresh sizes, and unlink or
/// rename invalidates the entry (and a directory's cached descendants).
pub fn cache_invalidation() -> Result<(), String> {
    let root = Id::ROOT;
    let mut vfs = ram_vfs();
    vfs.create(root, "/cache.txt", 0o644).map_err(fs_error)?;

    let first = vfs.stat(root, "/cache.txt").map_err(fs_error)?;
    let second = vfs.stat(root, "/cache.txt").map_err(fs_error)?;
    check!(first == second, "the two stats disagree");
    let stats = vfs.cache_stats();
    check!(
        stats.dentry_hits >= 1 && stats.inode_hits >= 1,
        "the caches did not warm: {stats:?}"
    );

    vfs.write(root, "/cache.txt", 0, b"123456")
        .map_err(fs_error)?;
    check!(
        vfs.stat(root, "/cache.txt").map_err(fs_error)?.size == 6,
        "the cached size is stale after a write"
    );

    vfs.unlink(root, "/cache.txt").map_err(fs_error)?;
    check!(
        vfs.stat(root, "/cache.txt").err() == Some(FsError::NotFound),
        "the unlinked entry is still cached"
    );
    check!(
        vfs.cache_stats().invalidations >= 1,
        "no invalidation was recorded"
    );

    // Renaming a directory drops its cached descendants: a stale path must
    // not keep resolving.
    vfs.mkdir(root, "/dir", 0o755).map_err(fs_error)?;
    vfs.create(root, "/dir/file", 0o644).map_err(fs_error)?;
    check!(
        vfs.stat(root, "/dir/file").is_ok(),
        "subtree did not resolve before rename"
    );
    vfs.rename(root, "/dir", "/dir2").map_err(fs_error)?;
    check!(
        vfs.stat(root, "/dir/file").err() == Some(FsError::NotFound),
        "a stale descendant survived the rename"
    );
    check!(
        vfs.stat(root, "/dir2/file").is_ok(),
        "the renamed subtree is missing"
    );

    // invalidate() is the explicit escape hatch and the next lookup refills.
    vfs.invalidate("/dir2/file");
    check!(
        vfs.stat(root, "/dir2/file").is_ok(),
        "the refill after invalidate failed"
    );
    Ok(())
}

/// The FAT boot volume is mounted read-only through the VFS (`/boot`; at `/` in
/// the legacy layout): reads work, and every mutating call answers EROFS with
/// the friendly message.
pub fn fat_read_only_erofs() -> Result<(), String> {
    task::register_kernel();
    check!(
        crate::fs::init(),
        "the FAT boot volume did not mount (is the disk image attached?)"
    );
    let root = Id::ROOT;
    let hello = format!("{}/hello.txt", fhs::share::SAMPLES);
    let meta = crate::fs::vfs_stat(root, &hello).map_err(fs_error)?;
    check!(
        meta.kind == FileKind::File && meta.size > 0,
        "{hello} metadata is {meta:?}"
    );
    let data = crate::fs::vfs_read(root, &hello).map_err(fs_error)?;
    check!(
        data.windows(17)
            .any(|window| window == b"Hello from LazyOS"),
        "{hello} contents are wrong"
    );
    // The root holds directories only (F3): the system tree among them.
    let listing = crate::fs::list();
    check!(
        listing
            .iter()
            .any(|(name, is_dir, _)| name == fhs::SYSTEM.trim_start_matches('/') && *is_dir),
        "the root listing is {listing:?}"
    );

    // The global umask is readable back (the `umask(2)` surface).
    let previous = crate::fs::vfs_set_umask(0o027);
    check!(
        crate::fs::vfs_umask() == 0o027,
        "the global umask did not stick"
    );
    check!(
        crate::fs::vfs_set_umask(previous) == 0o027,
        "umask did not return the previous value"
    );

    // The FAT volume is `/boot` in the configured layout (the OS volume is the
    // writable root) and the root itself in the legacy one.
    let (dir, file) = if crate::fs::vfs_stat(root, "/boot/lazyos.cfg").is_ok() {
        ("/boot", "/boot/lazyos.cfg")
    } else {
        ("", "/HELLO.TXT")
    };
    check!(
        crate::fs::vfs_write(root, file, 0, b"x").err() == Some(FsError::ReadOnly),
        "a FAT write was not EROFS"
    );
    check!(
        crate::fs::vfs_create(root, &format!("{dir}/NEW.TXT"), 0o644).err()
            == Some(FsError::ReadOnly),
        "a FAT create was not EROFS"
    );
    check!(
        crate::fs::vfs_mkdir(root, &format!("{dir}/newdir"), 0o755).err()
            == Some(FsError::ReadOnly),
        "a FAT mkdir was not EROFS"
    );
    check!(
        crate::fs::vfs_unlink(root, file).err() == Some(FsError::ReadOnly),
        "a FAT unlink was not EROFS"
    );
    check!(
        crate::fs::vfs_rename(root, file, &format!("{dir}/HI.TXT")).err()
            == Some(FsError::ReadOnly),
        "a FAT rename was not EROFS"
    );
    check!(
        FsError::ReadOnly.message().contains("read-only"),
        "the EROFS message is not friendly: {:?}",
        FsError::ReadOnly.message()
    );
    Ok(())
}

/// The Linux fd layer routes `openat`/`getdents64`/`fstat`/`close` through
/// the VFS: a ramfs directory on `/tmp` lists its real entries.
pub fn getdents64_ramfs_directory() -> Result<(), String> {
    task::register_kernel();
    crate::fs::init();
    let root = Id::ROOT;
    let dir = "/tmp/vfs-getdents";
    let _ = crate::fs::vfs_unlink(root, "/tmp/vfs-getdents/entry.txt");
    crate::fs::vfs_mkdir(root, dir, 0o755).map_err(fs_error)?;
    crate::fs::vfs_create(root, "/tmp/vfs-getdents/entry.txt", 0o644).map_err(fs_error)?;

    let path = b"/tmp/vfs-getdents\0";
    // dispatch_for_test(nr, a1, a2, a3): openat's a1 is `dirfd` (ignored)
    // and a2 is the path.
    let fd = process::linux::dispatch_for_test(257, 0, path.as_ptr() as u64, 0);
    check!(
        (3..task::fd_max() as u64).contains(&fd),
        "openat returned {fd:#x}"
    );

    let mut buf = [0u8; 512];
    let count =
        process::linux::dispatch_for_test(217, fd, buf.as_mut_ptr() as u64, buf.len() as u64);
    check!(
        count > 0 && count as usize <= buf.len(),
        "getdents64 returned {count}"
    );

    let mut names: Vec<String> = Vec::new();
    let mut offset = 0usize;
    while offset < count as usize {
        let reclen = u16::from_le_bytes([buf[offset + 16], buf[offset + 17]]) as usize;
        check!(
            reclen >= 19 && offset + reclen <= count as usize,
            "bad dirent record at offset {offset}"
        );
        let name = &buf[offset + 19..offset + reclen];
        let end = name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(name.len());
        names.push(String::from_utf8_lossy(&name[..end]).into_owned());
        offset += reclen;
    }
    check!(
        names.iter().any(|name| name == "."),
        "no `.` entry: {names:?}"
    );
    check!(
        names.iter().any(|name| name == ".."),
        "no `..` entry: {names:?}"
    );
    check!(
        names.iter().any(|name| name == "entry.txt"),
        "no entry.txt in the listing: {names:?}"
    );

    // fstat on the directory fd reports the VFS directory mode.
    let mut stat = [0u8; 144];
    let result = process::linux::dispatch_for_test(5, fd, stat.as_mut_ptr() as u64, 0);
    check!(result == 0, "fstat -> {result:#x}");
    let mode = u32::from_le_bytes([stat[24], stat[25], stat[26], stat[27]]);
    check!(
        mode & vfs::S_IFMT as u32 == 0o040000,
        "fstat mode is {mode:#o}, expected a directory"
    );

    let result = process::linux::dispatch_for_test(3, fd, 0, 0);
    check!(result == 0, "close -> {result:#x}");
    crate::fs::vfs_unlink(root, "/tmp/vfs-getdents/entry.txt").map_err(fs_error)?;
    Ok(())
}
