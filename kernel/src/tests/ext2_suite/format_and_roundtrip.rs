//! A miniature `mke2fs` fixture plus format/mount/create/write/
//! read/rename/unlink through the VFS.

use super::*;

/// Format, mount through the VFS, then run the whole op set: create,
/// write, read (offset, direct, and single-indirect), stat, mkdir,
/// readdir, rename (file and directory), unlink, sparse writes, and block
/// reuse after unlink.
pub fn create_write_read_rename_unlink() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;

    let meta = vfs.stat(root, "/").map_err(fs_error)?;
    check!(
        meta.kind == FileKind::Dir && meta.mode & vfs::S_IFMT == vfs::S_IFDIR,
        "root meta is {meta:?}"
    );

    // A directory and a small file living in direct blocks.
    vfs.mkdir(root, "/docs", 0o755).map_err(fs_error)?;
    vfs.create(root, "/docs/note.txt", 0o644)
        .map_err(fs_error)?;
    let meta = vfs.stat(root, "/docs/note.txt").map_err(fs_error)?;
    check!(
        meta.kind == FileKind::File && meta.size == 0 && meta.mode & vfs::S_IFMT == vfs::S_IFREG,
        "file meta is {meta:?}"
    );
    check!(
        vfs.write(root, "/docs/note.txt", 0, b"hello")
            .map_err(fs_error)?
            == 5,
        "the first write was short"
    );
    vfs.write(root, "/docs/note.txt", 5, b" world")
        .map_err(fs_error)?;
    check!(
        vfs.read_file(root, "/docs/note.txt").map_err(fs_error)? == b"hello world".to_vec(),
        "the file contents are wrong"
    );
    let mut buf = [0u8; 4];
    let read = vfs
        .read(root, "/docs/note.txt", 6, &mut buf)
        .map_err(fs_error)?;
    check!(
        read == 4 && &buf == b"worl",
        "offset read got {read} {buf:?}"
    );
    check!(
        vfs.read(root, "/docs/note.txt", 99, &mut buf)
            .map_err(fs_error)?
            == 0,
        "a read past EOF did not return 0"
    );
    check!(
        vfs.readdir(root, "/docs").map_err(fs_error)?.len() == 1,
        "readdir did not see exactly note.txt"
    );

    // A file past the twelve direct slots: the single indirect block must
    // appear, and an overwrite across the boundary must land correctly.
    let mut big = vec![0u8; 40 * 1024];
    for (index, byte) in big.iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    vfs.create(root, "/big.bin", 0o644).map_err(fs_error)?;
    vfs.write(root, "/big.bin", 0, &big).map_err(fs_error)?;
    check!(
        fs.mapped_block("/big.bin", 12).map_err(fs_error)? != 0,
        "the single indirect block was not allocated"
    );
    check!(
        vfs.read_file(root, "/big.bin").map_err(fs_error)? == big,
        "the 40 KiB round trip differs"
    );
    let patch = 12 * 1024 - 8;
    vfs.write(root, "/big.bin", patch as u64, &[0xAB; 32])
        .map_err(fs_error)?;
    big[patch..patch + 32].fill(0xAB);
    check!(
        vfs.read_file(root, "/big.bin").map_err(fs_error)? == big,
        "the overwrite across the indirect boundary differs"
    );

    // A sparse write leaves a hole that reads back as zeros.
    vfs.create(root, "/sparse", 0o644).map_err(fs_error)?;
    vfs.write(root, "/sparse", 5000, b"tail")
        .map_err(fs_error)?;
    check!(
        fs.mapped_block("/sparse", 0).map_err(fs_error)? == 0,
        "the sparse write allocated its hole block"
    );
    let sparse = vfs.read_file(root, "/sparse").map_err(fs_error)?;
    check!(
        sparse.len() == 5004
            && sparse[..5000].iter().all(|&byte| byte == 0)
            && &sparse[5000..] == b"tail",
        "the sparse read is wrong"
    );

    // Rename keeps contents; directories move with their subtrees.
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
    vfs.mkdir(root, "/docs/sub", 0o755).map_err(fs_error)?;
    vfs.create(root, "/docs/sub/inner", 0o644)
        .map_err(fs_error)?;
    let names: Vec<String> = vfs
        .readdir(root, "/docs")
        .map_err(fs_error)?
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    check!(names == ["memo.txt", "sub"], "readdir is {names:?}");
    vfs.rename(root, "/docs/sub", "/docs/moved")
        .map_err(fs_error)?;
    check!(
        vfs.stat(root, "/docs/sub/inner").err() == Some(FsError::NotFound),
        "the moved directory still resolves at the old path"
    );
    check!(
        vfs.stat(root, "/docs/moved/inner").is_ok(),
        "the moved directory's child is missing"
    );
    vfs.unlink(root, "/docs/moved/inner").map_err(fs_error)?;
    vfs.unlink(root, "/docs/memo.txt").map_err(fs_error)?;
    check!(
        vfs.readdir(root, "/docs").map_err(fs_error)?.len() == 1,
        "unlink left entries behind"
    );

    // Freeing a file returns its blocks to the bitmap, and the next file
    // reuses them (first-fit allocation).
    let baseline = fs.free_blocks().map_err(fs_error)?;
    let baseline_inodes = fs.free_inodes().map_err(fs_error)?;
    vfs.create(root, "/reuse.bin", 0o644).map_err(fs_error)?;
    vfs.write(root, "/reuse.bin", 0, &[0x11; 8 * 1024])
        .map_err(fs_error)?;
    let first = fs.mapped_block("/reuse.bin", 0).map_err(fs_error)?;
    let after = fs.free_blocks().map_err(fs_error)?;
    check!(
        after == baseline - 8,
        "reuse.bin took {} blocks (baseline {baseline}, after {after}, first {first})",
        baseline - after
    );
    check!(
        fs.free_inodes().map_err(fs_error)? == baseline_inodes - 1,
        "reuse.bin did not take an inode"
    );
    vfs.unlink(root, "/reuse.bin").map_err(fs_error)?;
    check!(
        fs.free_blocks().map_err(fs_error)? == baseline,
        "unlink did not return the blocks"
    );
    check!(
        fs.free_inodes().map_err(fs_error)? == baseline_inodes,
        "unlink did not return the inode"
    );
    vfs.create(root, "/reuse2.bin", 0o644).map_err(fs_error)?;
    vfs.write(root, "/reuse2.bin", 0, &[0x22; 8 * 1024])
        .map_err(fs_error)?;
    check!(
        fs.mapped_block("/reuse2.bin", 0).map_err(fs_error)? == first,
        "the freed block was not reused"
    );
    check!(
        fs.free_blocks().map_err(fs_error)? == baseline - 8,
        "reuse accounting is off"
    );
    vfs.unlink(root, "/reuse2.bin").map_err(fs_error)?;
    check!(
        fs.free_blocks().map_err(fs_error)? == baseline,
        "the final free count is wrong"
    );

    // Error paths.
    check!(
        vfs.create(root, "/docs", 0o644).err() == Some(FsError::Exists),
        "create replaced a directory"
    );
    check!(
        vfs.write(root, "/docs", 0, b"x").err() == Some(FsError::IsDir),
        "write succeeded on a directory"
    );
    check!(
        vfs.unlink(root, "/docs").err() == Some(FsError::IsDir),
        "unlink removed a directory"
    );
    check!(
        vfs.rename(root, "/nope", "/docs/x").err() == Some(FsError::NotFound),
        "rename found a ghost"
    );
    check!(
        vfs.create(root, "/missing/file", 0o644).err() == Some(FsError::NotFound),
        "create succeeded in a missing directory"
    );
    let long = "x".repeat(256);
    check!(
        vfs.create(root, &format!("/{long}"), 0o644).err() == Some(FsError::NameTooLong),
        "an over-long name was accepted"
    );

    // flush reaches the device and stamps the superblock.
    let before = disk.flushes.load(Ordering::Relaxed);
    fs.flush().map_err(fs_error)?;
    check!(
        disk.flushes.load(Ordering::Relaxed) == before + 1,
        "flush did not reach the block device"
    );
    Ok(())
}

/// The `regd` persist shape: a complete temporary file renamed over an
/// existing file. The destination must read back as the new contents only
/// (never a mix), the temporary name must be gone, and the `Vfs::flush`
/// passthrough `regd` uses must reach the block device.
///
/// This pins the property `libs/regd::persist` relies on. ext2 has no journal,
/// so a power loss *during* the rename is still only guaranteed to leave the
/// old or the new directory entry, not a torn file; the in-memory ramfs (the
/// fallback `regd` uses on this image) replaces under one lock and is crash
/// atomic within the boot.
pub fn rename_over_existing_replaces() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 256)?;
    let root = Id::ROOT;

    vfs.create(root, "/store", 0o644).map_err(fs_error)?;
    vfs.write(root, "/store", 0, b"old store bytes")
        .map_err(fs_error)?;
    vfs.create(root, "/store.tmp", 0o644).map_err(fs_error)?;
    vfs.write(root, "/store.tmp", 0, b"new store bytes")
        .map_err(fs_error)?;
    fs.flush().map_err(fs_error)?;

    vfs.rename(root, "/store.tmp", "/store").map_err(fs_error)?;

    check!(
        vfs.read_file(root, "/store").map_err(fs_error)? == b"new store bytes".to_vec(),
        "rename did not replace the destination"
    );
    check!(
        vfs.stat(root, "/store.tmp").err() == Some(FsError::NotFound),
        "the temporary name survived the rename"
    );

    let before = disk.flushes.load(Ordering::Relaxed);
    vfs.flush(root, "/store").map_err(fs_error)?;
    check!(
        disk.flushes.load(Ordering::Relaxed) == before + 1,
        "Vfs::flush did not reach the block device"
    );
    Ok(())
}

/// A directory renamed over an *empty* directory must release the victim's
/// inode and block, and the victim's `..` link on its parent (issue #298).
/// An empty directory has two links (`.` and its parent's entry), so the old
/// "free only at one link" rule kept it allocated and leaked both.
pub fn rename_dir_over_empty_dir_frees_victim() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, _disk) = mounted(1024, 256)?;
    let root = Id::ROOT;

    // Same parent: /a replaces the empty /b.
    vfs.mkdir(root, "/a", 0o755).map_err(fs_error)?;
    vfs.mkdir(root, "/b", 0o755).map_err(fs_error)?;
    let blocks = fs.free_blocks().map_err(fs_error)?;
    let inodes = fs.free_inodes().map_err(fs_error)?;
    check!(
        fs.link_count("/").map_err(fs_error)? == 4,
        "root links before"
    );
    vfs.rename(root, "/a", "/b").map_err(fs_error)?;
    check!(
        fs.free_blocks().map_err(fs_error)? == blocks + 1,
        "the replaced directory's block leaked"
    );
    check!(
        fs.free_inodes().map_err(fs_error)? == inodes + 1,
        "the replaced directory's inode leaked"
    );
    check!(
        fs.link_count("/").map_err(fs_error)? == 3,
        "the parent kept the replaced directory's `..` link"
    );
    check!(
        vfs.stat(root, "/a").err() == Some(FsError::NotFound),
        "the source name survived"
    );
    check!(
        vfs.stat(root, "/b").map_err(fs_error)?.kind == FileKind::Dir,
        "the destination is not a directory"
    );

    // Across parents: /p/x replaces the empty /q/y.
    vfs.mkdir(root, "/p", 0o755).map_err(fs_error)?;
    vfs.mkdir(root, "/q", 0o755).map_err(fs_error)?;
    vfs.mkdir(root, "/p/x", 0o755).map_err(fs_error)?;
    vfs.mkdir(root, "/q/y", 0o755).map_err(fs_error)?;
    let blocks = fs.free_blocks().map_err(fs_error)?;
    let inodes = fs.free_inodes().map_err(fs_error)?;
    vfs.rename(root, "/p/x", "/q/y").map_err(fs_error)?;
    check!(
        fs.free_blocks().map_err(fs_error)? == blocks + 1
            && fs.free_inodes().map_err(fs_error)? == inodes + 1,
        "cross-parent rename leaked the replaced directory"
    );
    check!(
        fs.link_count("/p").map_err(fs_error)? == 2,
        "the old parent kept the moved directory's link"
    );
    check!(
        fs.link_count("/q").map_err(fs_error)? == 3,
        "the new parent's link count is wrong (victim `..` not dropped, or moved `..` not added)"
    );

    // Unchanged rules: a non-empty victim is refused and nothing moves.
    vfs.mkdir(root, "/q/y/inner", 0o755).map_err(fs_error)?;
    vfs.mkdir(root, "/z", 0o755).map_err(fs_error)?;
    check!(
        vfs.rename(root, "/z", "/q/y").err() == Some(FsError::NotEmpty),
        "renaming over a non-empty directory was not refused"
    );
    check!(vfs.stat(root, "/z").is_ok(), "the refused source vanished");
    Ok(())
}

/// Soak for the same leak: each round creates a directory and renames it over
/// an existing empty one. The 64-inode test disk exhausts long before the
/// loop ends if any round leaks an inode or a block.
pub fn soak_rename_dir_over_empty_dir() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, _disk) = mounted(1024, 256)?;
    let root = Id::ROOT;

    vfs.mkdir(root, "/dst", 0o755).map_err(fs_error)?;
    let blocks = fs.free_blocks().map_err(fs_error)?;
    let inodes = fs.free_inodes().map_err(fs_error)?;
    let links = fs.link_count("/").map_err(fs_error)?;
    for round in 0..300 {
        vfs.mkdir(root, "/src", 0o755)
            .map_err(|e| format!("round {round}: mkdir: {}", fs_error(e)))?;
        vfs.rename(root, "/src", "/dst")
            .map_err(|e| format!("round {round}: rename: {}", fs_error(e)))?;
    }
    check!(
        fs.free_blocks().map_err(fs_error)? == blocks,
        "blocks leaked over the soak"
    );
    check!(
        fs.free_inodes().map_err(fs_error)? == inodes,
        "inodes leaked over the soak"
    );
    check!(
        fs.link_count("/").map_err(fs_error)? == links,
        "root link count drifted over the soak"
    );
    Ok(())
}
