//! The upper layer's byte/node cap, a create/write/rename/unlink
//! soak, and the Linux `*` syscalls including unlink-while-open.

use super::*;

/// The upper layer is capped: byte and node growth past the limit answers
/// `NoSpace`, and removing everything returns usage to the baseline.
pub fn enospc_limits() -> Result<(), String> {
    let lower = lower_fixture()?;
    let overlay = Arc::new(Overlay::with_limits(lower, 8, 4));
    let mut vfs = Vfs::new();
    vfs.mount("/", overlay.clone(), crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    let root = Id::ROOT;
    let baseline = overlay.usage();

    vfs.create(root, "/A.TXT", 0o644).map_err(fs_error)?;
    vfs.write(root, "/A.TXT", 0, b"12345678")
        .map_err(fs_error)?;
    check!(
        vfs.write(root, "/A.TXT", 8, b"9").err() == Some(FsError::NoSpace),
        "byte cap was not enforced"
    );
    check!(
        vfs.truncate(root, "/A.TXT", 9).err() == Some(FsError::NoSpace),
        "truncate cap was not enforced"
    );
    check!(
        vfs.read_file(root, "/A.TXT").map_err(fs_error)? == b"12345678",
        "a refused write changed the file"
    );

    // Node cap: root + A occupy two, B and C fill the four, D is refused.
    vfs.create(root, "/B.TXT", 0o644).map_err(fs_error)?;
    vfs.create(root, "/C.TXT", 0o644).map_err(fs_error)?;
    check!(
        vfs.create(root, "/D.TXT", 0o644).err() == Some(FsError::NoSpace),
        "node cap was not enforced"
    );

    // Cleanup returns to baseline exactly.
    vfs.unlink(root, "/A.TXT").map_err(fs_error)?;
    check!(
        overlay.usage().0 == 0,
        "unlink did not release the bytes: {:?}",
        overlay.usage()
    );
    vfs.unlink(root, "/B.TXT").map_err(fs_error)?;
    vfs.unlink(root, "/C.TXT").map_err(fs_error)?;
    check!(
        overlay.usage() == baseline,
        "upper scratch did not return to baseline: {:?}",
        overlay.usage()
    );
    Ok(())
}

/// Soak: many create/write/rename/unlink and mkdir/rmdir generations must
/// leave no upper bytes or nodes behind, and lower-file rename churn must
/// stay bounded (the copy-up persists exactly once).
pub fn soak_generations() -> Result<(), String> {
    const GENERATIONS: usize = 128;
    let lower = lower_fixture()?;
    let (mut vfs, overlay) = mounted_overlay(lower.clone())?;
    let root = Id::ROOT;
    let baseline = overlay.usage();

    for generation in 0..GENERATIONS {
        let file = format!("/GEN{generation}.TXT");
        let moved = format!("/MOVED{generation}.TXT");
        let dir = format!("/GDIR{generation}");
        let inner = format!("/GDIR{generation}/INNER.TXT");

        vfs.create(root, &file, 0o644).map_err(fs_error)?;
        vfs.write(root, &file, 0, b"payload").map_err(fs_error)?;
        check!(
            vfs.stat(root, &file).map_err(fs_error)?.size == 7,
            "generation {generation}: size is stale"
        );
        vfs.rename(root, &file, &moved).map_err(fs_error)?;
        check!(
            vfs.read_file(root, &moved).map_err(fs_error)? == b"payload",
            "generation {generation}: renamed bytes differ"
        );
        vfs.unlink(root, &moved).map_err(fs_error)?;

        vfs.mkdir(root, &dir, 0o755).map_err(fs_error)?;
        vfs.create(root, &inner, 0o644).map_err(fs_error)?;
        vfs.write(root, &inner, 0, b"x").map_err(fs_error)?;
        vfs.unlink(root, &inner).map_err(fs_error)?;
        vfs.rmdir(root, &dir).map_err(fs_error)?;

        check!(
            overlay.usage() == baseline,
            "generation {generation} leaked: {:?} -> {:?}",
            baseline,
            overlay.usage()
        );
    }

    // Lower-file rename churn copies up once; usage must not grow per pass.
    for _ in 0..32 {
        vfs.rename(root, "/HELLO.TXT", "/ABIREN.TXT")
            .map_err(fs_error)?;
        vfs.rename(root, "/ABIREN.TXT", "/HELLO.TXT")
            .map_err(fs_error)?;
    }
    let churn = overlay.usage();
    check!(
        churn.1 <= baseline.1 + 2 && churn.0 <= baseline.0 + 17,
        "lower rename churn grew unbounded: {churn:?} from {baseline:?}"
    );
    check!(
        vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello from LazyOS",
        "rename churn lost the contents"
    );
    Ok(())
}

/// The Linux `*` syscalls reach the ABI overlay: mkdir/rename/unlink/rmdir,
/// fd-relative `openat`/`unlinkat` (the `remove_dir_all` walk), and the
/// native table staying read-only.
pub fn abi_syscalls() -> Result<(), String> {
    const AT_FDCWD: u64 = (-100i64) as u64;
    const O_WRONLY: u64 = 1;
    const O_CREAT: u64 = 0o100;
    const O_EXCL: u64 = 0o200;
    const O_DIRECTORY: u64 = 0o200000;
    const AT_REMOVEDIR: u64 = 0x200;

    task::register_kernel();
    check!(crate::fs::init(), "the boot volume did not mount");

    // The legacy layout gives the ABI a copy-up overlay over the FAT root; the
    // configured one shares the ext2 OS volume with the native table, so
    // every mutation below is real and the native FAT volume is `/boot`.
    let mounts = crate::fs::abi_mounts();
    let overlay = mounts
        .iter()
        .any(|(point, name)| point == "/" && *name == "overlay (abi rw)");
    check!(
        overlay
            || mounts
                .iter()
                .any(|(point, name)| point == "/" && name.starts_with("ext2")),
        "ABI root is neither the overlay nor ext2: {mounts:?}"
    );
    check!(
        mounts
            .iter()
            .any(|(point, name)| point == "/tmp" && *name == "ramfs"),
        "ABI /tmp is not ramfs: {mounts:?}"
    );

    let dir = b"/ABIDIR\0";
    let sub = b"/ABIDIR/SUB\0";
    let from = b"/ABIDIR/SUB\0";
    let to = b"/ABIDIR/SUB2\0";
    let eexist = (-17i64) as u64;

    check!(
        process::linux::dispatch_for_test(83, dir.as_ptr() as u64, 0o755, 0) == 0,
        "mkdir failed"
    );
    check!(
        process::linux::dispatch_for_test(83, dir.as_ptr() as u64, 0o755, 0) == eexist,
        "mkdir over an existing directory did not answer EEXIST"
    );
    check!(
        process::linux::dispatch_for_test(258, AT_FDCWD, sub.as_ptr() as u64, 0o755) == 0,
        "mkdirat failed"
    );

    // fd-relative creation: open the directory, then create through it.
    let dirfd = process::linux::dispatch_for_test(257, AT_FDCWD, dir.as_ptr() as u64, O_DIRECTORY);
    check!(
        (3..task::fd_max() as u64).contains(&dirfd),
        "dirfd is {dirfd:#x}"
    );
    let child = b"CHILD.TXT\0";
    let childfd = process::linux::dispatch_for_test(
        257,
        dirfd,
        child.as_ptr() as u64,
        O_WRONLY | O_CREAT | O_EXCL,
    );
    check!(
        (3..task::fd_max() as u64).contains(&childfd),
        "fd-relative openat is {childfd:#x}"
    );
    check!(
        process::linux::dispatch_for_test(3, childfd, 0, 0) == 0,
        "close child failed"
    );
    check!(
        process::linux::dispatch_for_test(263, dirfd, child.as_ptr() as u64, 0) == 0,
        "fd-relative unlinkat failed"
    );
    check!(
        process::linux::dispatch_for_test(3, dirfd, 0, 0) == 0,
        "close dir failed"
    );

    // rename + rmdir through the plain syscalls.
    check!(
        process::linux::dispatch_for_test(82, from.as_ptr() as u64, to.as_ptr() as u64, 0) == 0,
        "rename failed"
    );
    check!(
        process::linux::dispatch_for_test(263, AT_FDCWD, to.as_ptr() as u64, AT_REMOVEDIR) == 0,
        "unlinkat(AT_REMOVEDIR) failed"
    );
    check!(
        process::linux::dispatch_for_test(84, dir.as_ptr() as u64, 0, 0) == 0,
        "rmdir failed"
    );

    // The overlay-visible path is gone, and the native table never saw it.
    check!(
        crate::fs::abi_stat(Id::ROOT, "/ABIDIR").err() == Some(FsError::NotFound),
        "ABIDIR still resolves through the ABI"
    );
    check!(
        crate::fs::vfs_stat(Id::ROOT, "/ABIDIR").err() == Some(FsError::NotFound),
        "ABIDIR leaked into the native table"
    );
    let native_ro = if overlay {
        "/NATIVE.TXT"
    } else {
        "/boot/NATIVE.TXT"
    };
    check!(
        crate::fs::vfs_create(Id::ROOT, native_ro, 0o644).err() == Some(FsError::ReadOnly),
        "the native FAT volume is no longer read-only"
    );
    Ok(())
}

/// `unlink` while a descriptor is open: the path stops resolving and the fd
/// keeps reading its data. On the overlay (a snapshot descriptor) a later write
/// through the orphan answers ENOENT, the documented snapshot-model gap; on the
/// ext2 root (an in-place descriptor on a hidden `.unlinked-*` entry) it
/// succeeds, as POSIX says.
pub fn unlink_while_open() -> Result<(), String> {
    const AT_FDCWD: u64 = (-100i64) as u64;
    // Read-write: the in-place descriptor enforces the access mode (reading a
    // write-only one is EBADF), where the snapshot one did not.
    const O_RDWR: u64 = 2;
    const O_CREAT: u64 = 0o100;
    const O_TRUNC: u64 = 0o1000;
    let enoent = (-2i64) as u64;

    task::register_kernel();
    check!(crate::fs::init(), "the boot volume did not mount");
    let overlay = crate::fs::abi_mounts()
        .iter()
        .any(|(point, name)| point == "/" && *name == "overlay (abi rw)");
    let path = b"/ABIOPEN.TXT\0";
    let _ = crate::fs::abi_unlink(Id::ROOT, "/ABIOPEN.TXT");

    let fd = process::linux::dispatch_for_test(
        257,
        AT_FDCWD,
        path.as_ptr() as u64,
        O_RDWR | O_CREAT | O_TRUNC,
    );
    check!(
        (3..task::fd_max() as u64).contains(&fd),
        "openat is {fd:#x}"
    );
    let payload = b"still readable";
    check!(
        process::linux::dispatch_for_test(1, fd, payload.as_ptr() as u64, payload.len() as u64)
            == payload.len() as u64,
        "write failed"
    );
    let mut stat = [0u8; 144];
    check!(
        process::linux::dispatch_for_test(5, fd, stat.as_mut_ptr() as u64, 0) == 0,
        "fstat failed"
    );
    check!(
        u64::from_le_bytes(stat[48..56].try_into().unwrap()) == payload.len() as u64,
        "fstat size is stale"
    );

    check!(
        process::linux::dispatch_for_test(87, path.as_ptr() as u64, 0, 0) == 0,
        "unlink failed"
    );
    check!(
        crate::fs::abi_stat(Id::ROOT, "/ABIOPEN.TXT").err() == Some(FsError::NotFound),
        "unlinked path still resolves"
    );

    // The open descriptor still reads its snapshot...
    check!(
        process::linux::dispatch_for_test(8, fd, 0, 0) == 0,
        "lseek failed"
    );
    let mut buf = [0u8; 32];
    let read = process::linux::dispatch_for_test(0, fd, buf.as_mut_ptr() as u64, buf.len() as u64);
    check!(
        read == payload.len() as u64 && &buf[..payload.len()] == payload,
        "the open fd lost its snapshot: read={read}"
    );
    // ...and a write through the orphan has no backing path on the overlay,
    // while the ext2 descriptor keeps writing its hidden entry.
    let wrote = process::linux::dispatch_for_test(1, fd, payload.as_ptr() as u64, 1);
    check!(
        wrote == if overlay { enoent } else { 1 },
        "writing through an unlinked fd answered {wrote:#x}"
    );
    check!(
        process::linux::dispatch_for_test(3, fd, 0, 0) == 0,
        "close failed"
    );
    Ok(())
}
