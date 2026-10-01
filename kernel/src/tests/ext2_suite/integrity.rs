//! Block-size variants, rejecting corrupt images, and the
//! `mount <dev>` surface wiring a registered device.

use super::*;

/// 1K, 2K, and 4K blocks all mount and round-trip a file that spills into
/// the single-indirect map, so every block-size-dependent shift is used.
pub fn block_sizes() -> Result<(), String> {
    task::register_kernel();
    for block_size in [1024u32, 2048, 4096] {
        let total_blocks = (DISK_SECTORS * SECTOR_SIZE) as u32 / block_size;
        let (fs, mut vfs, _disk) = mounted(block_size, total_blocks)?;
        check!(
            fs.block_size() == block_size,
            "open reported {} for a {block_size}-byte block",
            fs.block_size()
        );
        let root = Id::ROOT;
        vfs.mkdir(root, "/dir", 0o755).map_err(fs_error)?;
        vfs.create(root, "/dir/file", 0o644).map_err(fs_error)?;
        let payload: Vec<u8> = (0..60 * 1024).map(|index| (index % 253) as u8).collect();
        vfs.write(root, "/dir/file", 0, &payload)
            .map_err(fs_error)?;
        check!(
            fs.mapped_block("/dir/file", 12).map_err(fs_error)? != 0,
            "{block_size}: the indirect block is missing"
        );
        check!(
            vfs.read_file(root, "/dir/file").map_err(fs_error)? == payload,
            "{block_size}: the round trip differs"
        );
        // Rewriting the tail in place (no new allocation) must preserve
        // the leading blocks.
        let tail = payload.len() - 5;
        vfs.write(root, "/dir/file", tail as u64, b"12345")
            .map_err(fs_error)?;
        let mut expected = payload.clone();
        expected[tail..].copy_from_slice(b"12345");
        check!(
            vfs.read_file(root, "/dir/file").map_err(fs_error)? == expected,
            "{block_size}: the in-place rewrite differs"
        );
        vfs.unlink(root, "/dir/file").map_err(fs_error)?;
    }
    Ok(())
}

/// Malformed images answer friendly errors instead of panicking: bad
/// magic, unsupported geometry, impossible counts, unsupported features,
/// a truncated device, out-of-range group pointers, and a corrupt
/// directory record.
pub fn rejects_corruption() -> Result<(), String> {
    task::register_kernel();
    let good = mkfs(1024, 512, 64);
    let disk = FakeDisk::new("test-ext2-bad", DISK_SECTORS);

    // Swap an image into the shared disk and try to open it.
    let open_err = |image: &[u8]| -> Option<FsError> {
        disk.data.lock().copy_from_slice(image);
        Ext2::open(disk).err()
    };

    let mut bad = good.clone();
    bad[SUPER + 0x38] ^= 0xFF; // wrong magic
    check!(
        open_err(&bad) == Some(FsError::Invalid),
        "bad magic accepted"
    );

    let mut bad = good.clone();
    put32(&mut bad, SUPER + 0x18, 5); // log block size -> 32 KiB
    check!(
        open_err(&bad) == Some(FsError::Invalid),
        "an over-large block size was accepted"
    );

    let mut bad = good.clone();
    put16(&mut bad, SUPER + 0x58, 0); // zero inode size
    check!(
        open_err(&bad) == Some(FsError::Invalid),
        "a zero inode size was accepted"
    );

    let mut bad = good.clone();
    put32(&mut bad, SUPER + 0x60, 0x80); // incompat 64BIT
    check!(
        open_err(&bad) == Some(FsError::NotSupported),
        "the 64BIT feature was accepted"
    );

    let mut bad = good.clone();
    put32(&mut bad, SUPER + 0x64, 0x10); // ro-compat GDT_CSUM
    check!(
        open_err(&bad) == Some(FsError::NotSupported),
        "an unknown ro-compat feature was accepted"
    );

    let mut bad = good.clone();
    put32(&mut bad, SUPER + 0x04, 0xFFFF_FFFF); // block count past the device
    check!(
        open_err(&bad) == Some(FsError::Invalid),
        "an impossible block count was accepted"
    );

    let mut bad = good.clone();
    put32(&mut bad, SUPER + 0x10, 1000); // more free inodes than inodes
    check!(
        open_err(&bad) == Some(FsError::Invalid),
        "free inodes above the total were accepted"
    );

    // A device too short to hold even the superblock.
    let tiny = FakeDisk::new("test-ext2-tiny", 1);
    check!(
        Ext2::open(tiny).err() == Some(FsError::Invalid),
        "a one-sector device was accepted"
    );

    // A valid superblock hiding a group descriptor whose bitmap pointer
    // lies outside the volume: open succeeds, the first read refuses.
    let mut bad = good.clone();
    put32(&mut bad, 2 * 1024, 0xFFFF_FFFF); // group 0 block bitmap pointer
    disk.data.lock().copy_from_slice(&bad);
    let fs = Arc::new(Ext2::open(disk).map_err(fs_error)?);
    let mut vfs = Vfs::new();
    vfs.mount("/", fs, crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    check!(
        vfs.stat(Id::ROOT, "/").err() == Some(FsError::Invalid),
        "an out-of-range group pointer was accepted"
    );

    // A valid superblock hiding a corrupt directory record: `readdir`
    // must answer Invalid rather than loop or panic.
    let mut bad = good.clone();
    // For 1K blocks and the mkfs layout, root data block 13 is in the
    // 512-block image (gdt 2, bitmaps 3/4, inode table 5..12, root 13).
    put16(&mut bad, 13 * 1024 + 4, 3); // record length not a multiple of 4
    disk.data.lock().copy_from_slice(&bad);
    let fs = Arc::new(Ext2::open(disk).map_err(fs_error)?);
    let mut vfs = Vfs::new();
    vfs.mount("/", fs, crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    check!(
        vfs.readdir(Id::ROOT, "/").err() == Some(FsError::Invalid),
        "a corrupt directory record was accepted"
    );
    Ok(())
}

/// The `mount <dev>` surface opens ext2 on any registered device (not
/// just the boot volume), and the mount is reachable through the global
/// VFS helpers.
pub fn mount_device_wiring() -> Result<(), String> {
    task::register_kernel();
    crate::fs::init();
    let image = mkfs(1024, 512, 64);
    let disk = FakeDisk::new("test-ext2-mount", DISK_SECTORS);
    disk.data.lock().copy_from_slice(&image);
    check!(
        block::register(disk).is_ok(),
        "registering the ext2 disk failed"
    );
    check!(
        crate::fs::mount_device("/ext2", "test-ext2-mount").is_ok(),
        "mount_device refused an ext2 volume"
    );
    check!(
        crate::fs::mount_device("/ext2", "test-ext2-mount").err() == Some(FsError::Exists),
        "a duplicate mount point was accepted"
    );

    let root = Id::ROOT;
    check!(
        crate::fs::vfs_stat(root, "/ext2").map_err(fs_error)?.kind == FileKind::Dir,
        "/ext2 is not a directory"
    );
    crate::fs::vfs_mkdir(root, "/ext2/home", 0o755).map_err(fs_error)?;
    crate::fs::vfs_create(root, "/ext2/home/file.txt", 0o644).map_err(fs_error)?;
    crate::fs::vfs_write(root, "/ext2/home/file.txt", 0, b"mounted ext2").map_err(fs_error)?;
    let data = crate::fs::vfs_read(root, "/ext2/home/file.txt").map_err(fs_error)?;
    check!(
        data == b"mounted ext2".to_vec(),
        "the global VFS read returned {data:?}"
    );
    crate::fs::vfs_unlink(root, "/ext2/home/file.txt").map_err(fs_error)?;
    Ok(())
}
