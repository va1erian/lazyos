//! Orphan reclaim at mount (issue #346): which volumes are scanned, what a
//! scan may and may not delete, and that the data mount runs it.

use super::*;

/// Park `body` under the reserved name `hidden`, as `unlink` of an open file
/// does (a rename within the volume; the VFS itself does not police names).
fn park(vfs: &mut Vfs, from: &str, hidden: &str, body: &[u8]) -> Result<(), String> {
    let root = Id::ROOT;
    vfs.create(root, from, 0o644).map_err(fs_error)?;
    vfs.write(root, from, 0, body).map_err(fs_error)?;
    vfs.rename(root, from, hidden).map_err(fs_error)
}

/// Names in `dir` that start with the reserved prefix.
fn parked_in(vfs: &mut Vfs, dir: &str) -> Result<usize, String> {
    let entries = vfs.readdir(Id::ROOT, dir).map_err(fs_error)?;
    Ok(entries
        .iter()
        .filter(|entry| entry.name.starts_with(".unlinked-"))
        .count())
}

/// The free `(blocks, inodes)` the superblock reports.
fn free_counts(fs: &Ext2) -> Result<(u32, u32), String> {
    Ok((
        fs.free_blocks().map_err(fs_error)?,
        fs.free_inodes().map_err(fs_error)?,
    ))
}

/// A volume that stopped uncleanly with orphans in several directories: the
/// next mount deletes exactly those, returns every block and inode, and leaves
/// the live files alone.
pub fn orphans_reclaimed_on_unclean_mount() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    vfs.create(root, "/keep", 0o644).map_err(fs_error)?;
    vfs.write(root, "/keep", 0, &pattern_bytes(1, 3000))
        .map_err(fs_error)?;
    vfs.mkdir(root, "/d", 0o755).map_err(fs_error)?;
    vfs.mkdir(root, "/d/e", 0o755).map_err(fs_error)?;
    let baseline = free_counts(&fs)?;

    park(&mut vfs, "/y", "/.unlinked-1", &pattern_bytes(2, 5000))?;
    park(
        &mut vfs,
        "/d/e/x",
        "/d/e/.unlinked-3",
        &pattern_bytes(3, 20_000),
    )?;
    park(&mut vfs, "/d/z", "/d/.unlinked-4", &[])?;
    drop((fs, vfs)); // no flush: the volume is left flagged dirty

    let (fs, mut vfs) = remount_disk(disk)?;
    check!(!fs.was_clean_at_mount(), "the fixture is not unclean");
    check!(fs.reclaim_orphans() == 3, "not every orphan was reclaimed");
    for dir in ["/", "/d", "/d/e"] {
        check!(
            parked_in(&mut vfs, dir)? == 0,
            "a hidden entry survived in {dir}"
        );
    }
    check!(
        free_counts(&fs)? == baseline,
        "reclaim did not return every block and inode"
    );
    check!(
        vfs.read_file(root, "/keep").map_err(fs_error)? == pattern_bytes(1, 3000),
        "a live file was damaged"
    );
    check!(fs.reclaim_orphans() == 0, "a second scan found more");
    fs.flush().map_err(fs_error)?;
    check_volume(disk, 512)
}

/// A volume flagged clean is not scanned at all: byte-for-byte untouched.
pub fn clean_volume_is_not_scanned() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    park(&mut vfs, "/y", "/.unlinked-1", &pattern_bytes(2, 5000))?;
    fs.flush().map_err(fs_error)?;
    drop((fs, vfs));
    let image = disk.data.lock().clone();

    let (fs, mut vfs) = remount_disk(disk)?;
    check!(fs.was_clean_at_mount(), "the fixture is not clean");
    check!(fs.reclaim_orphans() == 0, "a clean volume was scanned");
    check!(
        parked_in(&mut vfs, "/")? == 1,
        "the entry on a clean volume vanished"
    );
    check!(*disk.data.lock() == image, "a clean volume was written");
    Ok(())
}

/// Only a regular file with the reserved prefix is deleted: a directory with
/// the name (and what is inside it), near misses, and ordinary files stay.
pub fn lookalikes_are_left_alone() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    vfs.mkdir(root, "/.unlinked-9", 0o755).map_err(fs_error)?;
    park(&mut vfs, "/inner", "/.unlinked-9/.unlinked-1", b"inside")?;
    vfs.mkdir(root, "/d", 0o755).map_err(fs_error)?;
    park(&mut vfs, "/orphan", "/d/.unlinked-5", b"gone")?;
    let survivors = [
        "/.unlinked",
        "/.unlinked_2",
        "/x.unlinked-1",
        "/unlinked-3",
        "/d/.unlinked",
    ];
    for name in survivors {
        vfs.create(root, name, 0o644).map_err(fs_error)?;
        vfs.write(root, name, 0, name.as_bytes())
            .map_err(fs_error)?;
    }
    drop((fs, vfs));

    let (fs, mut vfs) = remount_disk(disk)?;
    check!(
        fs.reclaim_orphans() == 1,
        "the reclaim count is not exactly the one real orphan"
    );
    check!(
        vfs.stat(root, "/d/.unlinked-5").err() == Some(FsError::NotFound),
        "the real orphan survived"
    );
    let meta = vfs.stat(root, "/.unlinked-9").map_err(fs_error)?;
    check!(meta.kind == FileKind::Dir, "the directory was replaced");
    check!(
        vfs.read_file(root, "/.unlinked-9/.unlinked-1")
            .map_err(fs_error)?
            == b"inside",
        "a file inside a reserved directory was touched"
    );
    for name in survivors {
        check!(
            vfs.read_file(root, name).map_err(fs_error)? == name.as_bytes(),
            "{name} was damaged"
        );
    }
    Ok(())
}

/// A table with a ramfs root, so `/data` has somewhere to mount.
fn table_with_root() -> Result<Vfs, String> {
    let mut table = Vfs::new();
    table
        .mount("/", Arc::new(crate::fs::ramfs::RamFs::new()))
        .map_err(fs_error)?;
    Ok(table)
}

/// Mount `disk` as `/data` and count the hidden entries visible there.
fn parked_after_data_mount(disk: &'static FakeDisk) -> Result<usize, String> {
    let devices: [&'static dyn block::BlockDevice; 1] = [disk];
    let mut table = table_with_root()?;
    check!(
        crate::fs::mount_data_volume(&mut table, None, &devices).is_some(),
        "the volume did not mount"
    );
    parked_in(&mut table, "/data")
}

/// The data mount reclaims before exposing the volume, and a read-only device
/// is never written.
pub fn data_mount_reclaims_before_exposure() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    park(&mut vfs, "/y", "/.unlinked-1", &pattern_bytes(2, 5000))?;
    drop((fs, vfs));

    disk.set_read_only(true);
    let image = disk.data.lock().clone();
    let read_only = parked_after_data_mount(disk);
    let untouched = *disk.data.lock() == image;
    disk.set_read_only(false); // whatever happened, the pooled disk is reusable
    check!(read_only? == 1, "a read-only mount deleted an orphan");
    check!(untouched, "a read-only device was written");

    check!(
        parked_after_data_mount(disk)? == 0,
        "the orphan is visible after the mount"
    );
    Ok(())
}
