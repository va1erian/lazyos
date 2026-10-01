//! Writes survive a remount (issue #5): the writable filesystem over the block
//! layer is the persistence path (the FAT boot volume is read-only by design).

use super::*;

/// Mount a fresh view of the same disk, as a reboot would.
fn remount(disk: &'static FakeDisk) -> Result<(Arc<Ext2>, Vfs), String> {
    let fs = Arc::new(Ext2::open(disk).map_err(fs_error)?);
    let mut vfs = Vfs::new();
    vfs.mount("/", fs.clone(), crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    Ok((fs, vfs))
}

/// Create a tree, flush, remount from the raw sectors, and find it intact.
pub fn files_survive_remount() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    vfs.mkdir(root, "/keep", 0o755).map_err(fs_error)?;
    vfs.create(root, "/keep/a.txt", 0o644).map_err(fs_error)?;
    vfs.write(root, "/keep/a.txt", 0, b"persisted")
        .map_err(fs_error)?;
    // A multi-block file crossing into the indirect block.
    let big: Vec<u8> = (0..20_000u32).map(|i| (i % 253) as u8).collect();
    vfs.create(root, "/big.bin", 0o644).map_err(fs_error)?;
    vfs.write(root, "/big.bin", 0, &big).map_err(fs_error)?;
    fs.flush().map_err(fs_error)?;
    drop(vfs);
    drop(fs);

    let (_fs, mut vfs) = remount(disk)?;
    check!(
        vfs.read_file(root, "/keep/a.txt").map_err(fs_error)? == b"persisted".to_vec(),
        "the small file changed across the remount"
    );
    check!(
        vfs.read_file(root, "/big.bin").map_err(fs_error)? == big,
        "the multi-block file changed across the remount"
    );
    // The remounted volume is still writable and consistent.
    vfs.unlink(root, "/keep/a.txt").map_err(fs_error)?;
    check!(
        vfs.readdir(root, "/keep").map_err(fs_error)?.is_empty(),
        "unlink after remount left an entry"
    );
    Ok(())
}

/// Soak: 150 generations of remount, verify the previous generation's file,
/// replace it, flush. Nothing is lost across any remount, and the small
/// volume never fills up (a block leak here would exhaust it long before the
/// end).
pub fn soak_remount_generations() -> Result<(), String> {
    task::register_kernel();
    let (fs, vfs, disk) = mounted(1024, 512)?;
    drop((fs, vfs));
    let root = Id::ROOT;
    let mut previous: Option<(String, Vec<u8>)> = None;
    for generation in 0..150u32 {
        let (fs, mut vfs) = remount(disk)?;
        if let Some((name, body)) = previous.take() {
            check!(
                vfs.read_file(root, &name).map_err(fs_error)? == body,
                "generation {generation}: {name} changed across a remount"
            );
            vfs.unlink(root, &name).map_err(fs_error)?;
        }
        let name = format!("/g{}.dat", generation % 5);
        let body: Vec<u8> = (0..(generation as usize * 37) % 3000)
            .map(|i| (i as u32 ^ generation) as u8)
            .collect();
        vfs.create(root, &name, 0o644).map_err(fs_error)?;
        vfs.write(root, &name, 0, &body).map_err(fs_error)?;
        fs.flush().map_err(fs_error)?;
        previous = Some((name, body));
    }
    Ok(())
}
