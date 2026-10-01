//! ramfs hardening (issues #227, #234): rename edge cases, the byte and node
//! caps, and soaks proving the caps hold and resources return to baseline.

use super::*;
use crate::fs::vfs::Filesystem;

/// A ramfs mounted at `/tmp` under a root ramfs, like the real layout.
fn tmp_vfs() -> Vfs {
    let mut vfs = ram_vfs();
    vfs.mount(
        "/tmp",
        Arc::new(RamFs::new()),
        crate::fs::vfs::MountFlags::default(),
    )
    .expect("mount ramfs at /tmp");
    vfs
}

/// `rename(a, a)` in any spelling is a no-op that keeps the file, and a
/// directory cannot move beneath itself (it used to orphan the subtree).
pub fn ramfs_rename_same_path_and_cycles() -> Result<(), String> {
    let root = Id::ROOT;
    let mut vfs = tmp_vfs();
    vfs.mkdir(root, "/tmp/a", 0o755).map_err(fs_error)?;
    vfs.mkdir(root, "/tmp/a/b", 0o755).map_err(fs_error)?;
    vfs.create(root, "/tmp/f", 0o644).map_err(fs_error)?;
    vfs.write(root, "/tmp/f", 0, b"keep").map_err(fs_error)?;

    for (from, to) in [
        ("/tmp/f", "/tmp/f"),
        ("/tmp/f", "/tmp//f"),
        ("/tmp/f", "/tmp/./x/../f"),
    ] {
        vfs.rename(root, from, to).map_err(fs_error)?;
        check!(
            vfs.read_file(root, "/tmp/f").map_err(fs_error)? == b"keep",
            "{from} -> {to} lost the file"
        );
    }
    vfs.rename(root, "/tmp/a", "/tmp/a").map_err(fs_error)?;

    for to in ["/tmp/a/b/c", "/tmp/a/x", "/tmp/a/b"] {
        check!(
            vfs.rename(root, "/tmp/a", to) == Err(FsError::Invalid),
            "moving /tmp/a under itself ({to}) was not refused"
        );
    }
    check!(
        vfs.stat(root, "/tmp/a/b").is_ok(),
        "the subtree was orphaned by a refused rename"
    );
    vfs.rename(root, "/tmp/a/b", "/tmp/c").map_err(fs_error)?;
    check!(vfs.stat(root, "/tmp/c").is_ok(), "a legal move failed");

    // The raw filesystem holds the same guards (the VFS ones are not enough
    // for a `Filesystem` used on its own).
    let ram = RamFs::new();
    let owner = Id::ROOT;
    ram.create("f", 0o644, owner).map_err(fs_error)?;
    ram.mkdir("d", 0o755, owner).map_err(fs_error)?;
    ram.mkdir("d/e", 0o755, owner).map_err(fs_error)?;
    ram.rename("f", "f").map_err(fs_error)?;
    ram.rename("/f", "f").map_err(fs_error)?;
    check!(ram.lookup("f").is_ok(), "self-rename dropped the file");
    check!(
        ram.rename("d", "d/e/x") == Err(FsError::Invalid),
        "raw ramfs allowed a directory under itself"
    );
    check!(ram.usage() == (0, 4), "usage drifted: {:?}", ram.usage());
    Ok(())
}

/// The byte cap turns overflow into `NoSpace`, leaves data and accounting
/// intact, and never allocates for absurd offsets or sizes.
pub fn ramfs_byte_cap_enospc() -> Result<(), String> {
    let ram = RamFs::with_limits(1024, 16);
    let owner = Id::ROOT;
    ram.create("f", 0o644, owner).map_err(fs_error)?;
    ram.write("f", 0, &[7u8; 1000]).map_err(fs_error)?;
    check!(
        ram.write("f", 1000, &[1u8; 100]) == Err(FsError::NoSpace),
        "write past the cap succeeded"
    );
    check!(ram.usage().0 == 1000, "failed write moved the accounting");
    check!(
        ram.write("f", 1 << 40, b"x") == Err(FsError::NoSpace)
            && ram.write("f", u64::MAX, b"x") == Err(FsError::NoSpace),
        "huge sparse offset was not refused"
    );
    check!(
        ram.truncate("f", 1 << 40) == Err(FsError::NoSpace)
            && ram.truncate("f", 1025) == Err(FsError::NoSpace),
        "truncate past the cap succeeded"
    );
    let mut back = [0u8; 1000];
    ram.read("f", 0, &mut back).map_err(fs_error)?;
    check!(
        back.iter().all(|&b| b == 7),
        "refused writes corrupted data"
    );

    // Overwrite in place needs no new room; shrinking frees it for others.
    ram.write("f", 0, &[9u8; 1000]).map_err(fs_error)?;
    ram.truncate("f", 10).map_err(fs_error)?;
    ram.create("g", 0o644, owner).map_err(fs_error)?;
    ram.write("g", 0, &[1u8; 1014]).map_err(fs_error)?;
    check!(ram.usage().0 == 1024, "usage {:?}", ram.usage());
    check!(
        ram.write("g", 1014, b"x") == Err(FsError::NoSpace),
        "cap is exact"
    );
    ram.unlink("g").map_err(fs_error)?;
    ram.unlink("f").map_err(fs_error)?;
    check!(ram.usage() == (0, 1), "unlink left usage {:?}", ram.usage());
    Ok(())
}

/// The node cap bounds files and directories, and rename-over-existing gives
/// its node back.
pub fn ramfs_node_cap_enospc() -> Result<(), String> {
    let ram = RamFs::with_limits(usize::MAX, 4); // root + 3
    let owner = Id::ROOT;
    ram.create("a", 0o644, owner).map_err(fs_error)?;
    ram.mkdir("d", 0o755, owner).map_err(fs_error)?;
    ram.create("b", 0o644, owner).map_err(fs_error)?;
    check!(
        ram.create("c", 0o644, owner).err() == Some(FsError::NoSpace)
            && ram.mkdir("e", 0o755, owner).err() == Some(FsError::NoSpace),
        "node cap not enforced"
    );
    ram.rename("a", "b").map_err(fs_error)?; // replaces b: frees a node
    ram.create("c", 0o644, owner).map_err(fs_error)?;
    check!(ram.usage().1 == 4, "live nodes {}", ram.usage().1);
    Ok(())
}

/// Soak: fill a default-capped ramfs to `NoSpace` and drain it, over and
/// over. Accounting must never exceed the cap, and every drain must return
/// the filesystem to its empty baseline (no stranded bytes or nodes).
pub fn ramfs_soak_fill_and_drain() -> Result<(), String> {
    use crate::fs::ramfs::DEFAULT_MAX_BYTES;
    let ram = RamFs::new();
    let owner = Id::ROOT;
    let chunk = alloc::vec![0xa5u8; 64 * 1024];
    for round in 0..24 {
        let mut files = 0usize;
        loop {
            let name = format!("f{files}");
            ram.create(&name, 0o644, owner).map_err(fs_error)?;
            match ram.write(&name, 0, &chunk) {
                Ok(_) => files += 1,
                Err(FsError::NoSpace) => {
                    ram.unlink(&name).map_err(fs_error)?;
                    break;
                }
                Err(other) => return Err(fs_error(other)),
            }
            let (bytes, _) = ram.usage();
            check!(
                bytes <= DEFAULT_MAX_BYTES,
                "round {round}: {bytes} bytes over cap"
            );
        }
        check!(
            files * chunk.len() == DEFAULT_MAX_BYTES,
            "round {round}: filled {} of {DEFAULT_MAX_BYTES}",
            files * chunk.len()
        );
        for i in 0..files {
            ram.unlink(&format!("f{i}")).map_err(fs_error)?;
        }
        check!(
            ram.usage() == (0, 1),
            "round {round}: usage {:?}",
            ram.usage()
        );
    }
    Ok(())
}

/// Descriptors that `dup`/`fork` share one snapshot instead of copying it,
/// and a write through one copy leaves the others untouched.
pub fn fd_snapshot_shared_and_cow() -> Result<(), String> {
    task::register_kernel();
    let data = Arc::new(alloc::vec![1u8; 512 * 1024]);
    let mut fds = Vec::new();
    for _ in 0..8 {
        let fd = task::fd_open(task::Fd::File {
            data: Arc::clone(&data),
            offset: 0,
        })
        .ok_or("fd_open failed")?;
        fds.push(fd);
    }
    check!(
        Arc::strong_count(&data) == 1 + fds.len(),
        "descriptors copied the snapshot (strong count {})",
        Arc::strong_count(&data)
    );
    check!(task::fd_apply_write(fds[0], 0, b"zz"), "apply_write failed");
    check!(
        Arc::strong_count(&data) == fds.len(),
        "write did not detach the writer's copy"
    );
    check!(
        task::fd_read(fds[1], 2) == Some(alloc::vec![1, 1]),
        "a sibling descriptor saw the write"
    );
    task::fd_seek(fds[0], 0, 0).ok_or("seek failed")?;
    check!(
        task::fd_read(fds[0], 2) == Some(b"zz".to_vec()),
        "the writer lost its own write"
    );
    for fd in fds {
        task::fd_close(fd);
    }
    check!(
        Arc::strong_count(&data) == 1,
        "closing leaked snapshot refs"
    );
    Ok(())
}
