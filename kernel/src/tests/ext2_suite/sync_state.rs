//! The clean/dirty superblock state, `sync_all`, and the `/data` mount probe.

use super::*;
use crate::block::BlockDevice;
use crate::fs::vfs::Filesystem;

/// A mounted volume starts clean, reading never dirties it, the first change
/// flags it dirty on disk immediately, and a flush restores clean (with the
/// data flushed before the marker and the marker flushed after).
pub fn state_dirty_then_clean() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    check!(
        raw_state(disk) & 1 == 1 && fs.was_clean_at_mount(),
        "a fresh volume is not clean"
    );

    let image = disk.data.lock().clone();
    vfs.readdir(root, "/").map_err(fs_error)?;
    vfs.stat(root, "/").map_err(fs_error)?;
    check!(*disk.data.lock() == image, "reading wrote to the disk");
    fs.flush().map_err(fs_error)?;
    check!(
        *disk.data.lock() == image,
        "syncing a clean, untouched volume wrote"
    );

    vfs.create(root, "/f", 0o644).map_err(fs_error)?;
    check!(
        raw_state(disk) & 1 == 0,
        "the first change did not flag the volume dirty"
    );
    vfs.write(root, "/f", 0, b"data").map_err(fs_error)?;
    check!(
        raw_state(disk) & 1 == 0,
        "the volume went clean without a sync"
    );

    let before = disk.flushes.load(Ordering::Relaxed);
    fs.flush().map_err(fs_error)?;
    check!(
        raw_state(disk) & 1 == 1,
        "a sync left the volume flagged dirty"
    );
    check!(
        disk.flushes.load(Ordering::Relaxed) == before + 2,
        "a dirty sync must flush before and after the clean marker"
    );
    check_volume(disk, 512)?;

    vfs.unlink(root, "/f").map_err(fs_error)?;
    check!(
        raw_state(disk) & 1 == 0,
        "a later change did not re-dirty the volume"
    );
    fs.flush().map_err(fs_error)?;
    check!(
        raw_state(disk) & 1 == 1,
        "the second sync did not mark clean"
    );
    Ok(())
}

/// A volume that arrives unclean is reported and stays that way (our own
/// shutdown must not launder it), recorded-error bits survive, and a clean
/// stop is what a remount then sees.
pub fn state_unclean_mount_is_not_laundered() -> Result<(), String> {
    task::register_kernel();
    let (fs, vfs, disk) = mounted(1024, 512)?;
    drop((fs, vfs));
    let root = Id::ROOT;

    // A previous session that never synced: valid bit clear.
    disk.data.lock()[SUPER + 0x3A] = 0;
    let (fs, mut vfs) = remount_disk(disk)?;
    check!(
        !fs.was_clean_at_mount(),
        "an unclean volume was reported clean"
    );
    vfs.create(root, "/f", 0o644).map_err(fs_error)?;
    fs.flush().map_err(fs_error)?;
    check!(
        raw_state(disk) == 0,
        "our sync blessed a volume we found unclean"
    );
    drop((fs, vfs));

    // Errors recorded by another OS are preserved across dirty and clean.
    disk.data.lock()[SUPER + 0x3A] = 3; // valid + error
    let (fs, mut vfs) = remount_disk(disk)?;
    check!(
        fs.was_clean_at_mount(),
        "valid+error should count as flagged valid"
    );
    vfs.unlink(root, "/f").map_err(fs_error)?;
    check!(
        raw_state(disk) == 2,
        "dirtying must clear only the valid bit"
    );
    fs.flush().map_err(fs_error)?;
    check!(
        raw_state(disk) == 3,
        "the sync did not restore the mount state"
    );
    drop((fs, vfs));

    // Cut power mid-session: the remount sees an unclean volume; after a
    // proper sync the next mount sees a clean one.
    disk.data.lock()[SUPER + 0x3A] = 1;
    let (fs, mut vfs) = remount_disk(disk)?;
    vfs.create(root, "/g", 0o644).map_err(fs_error)?;
    drop((fs, vfs)); // no sync: the power went out
    let (fs, _vfs) = remount_disk(disk)?;
    check!(
        !fs.was_clean_at_mount(),
        "an unsynced stop was not detected"
    );
    Ok(())
}

/// Failure of either marker write is handled honestly: a failed dirty marker
/// stops the change before anything else is written, and a failed clean
/// marker leaves the volume flagged dirty.
pub fn state_marker_write_failures() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    let image = disk.data.lock().clone();

    disk.fail_nth_write(1); // the dirty marker itself
    check!(
        vfs.create(root, "/f", 0o644).is_err(),
        "a change went ahead without its dirty marker"
    );
    check!(
        *disk.data.lock() == image,
        "the refused change still wrote to the disk"
    );
    vfs.create(root, "/f", 0o644).map_err(fs_error)?; // the fault was transient
    check!(
        raw_state(disk) & 1 == 0,
        "the retry did not flag the volume dirty"
    );

    disk.fail_nth_write(1); // the clean marker (the first write of the sync)
    check!(
        fs.flush().is_err(),
        "a failed clean marker was reported as success"
    );
    check!(
        raw_state(disk) & 1 == 0,
        "the volume claims clean after a failed marker"
    );
    fs.flush().map_err(fs_error)?;
    check!(
        raw_state(disk) & 1 == 1,
        "the retried sync did not mark clean"
    );
    check_volume(disk, 512)
}

/// `Vfs::sync_all` flushes every mount, marks each clean, and keeps going when
/// one of them fails, reporting the first error.
pub fn sync_all_flushes_every_mount() -> Result<(), String> {
    task::register_kernel();
    let (fs_a, _unused, disk_a) = mounted_in(0, 1024, 512)?;
    let (fs_b, _unused, disk_b) = mounted_in(1, 1024, 512)?;
    let root = Id::ROOT;
    let mut vfs = Vfs::new();
    vfs.mount("/", fs_a.clone()).map_err(fs_error)?;
    vfs.mount("/data", fs_b.clone()).map_err(fs_error)?;
    vfs.create(root, "/x", 0o644).map_err(fs_error)?;
    vfs.create(root, "/data/y", 0o644).map_err(fs_error)?;
    check!(
        raw_state(disk_a) & 1 == 0 && raw_state(disk_b) & 1 == 0,
        "both volumes should be dirty before the sync"
    );
    vfs.sync_all().map_err(fs_error)?;
    check!(
        raw_state(disk_a) & 1 == 1 && raw_state(disk_b) & 1 == 1,
        "sync_all left a volume dirty"
    );

    vfs.create(root, "/x2", 0o644).map_err(fs_error)?;
    vfs.create(root, "/data/y2", 0o644).map_err(fs_error)?;
    disk_a.fail_nth_write(1); // the first mount's clean marker fails
    check!(vfs.sync_all().is_err(), "a failing mount was not reported");
    check!(
        raw_state(disk_a) & 1 == 0 && raw_state(disk_b) & 1 == 1,
        "one failing mount stopped the others from being synced"
    );
    vfs.sync_all().map_err(fs_error)?;
    check!(
        raw_state(disk_a) & 1 == 1,
        "the retry did not clean the failed mount"
    );
    crate::fs::init();
    check!(crate::fs::sync_all().is_ok(), "the global sync_all failed");
    Ok(())
}

/// Format `disk` as a one-group ext2 volume.
fn format(disk: &'static FakeDisk) {
    disk.data.lock().copy_from_slice(&mkfs(1024, 512, 64));
}

/// `mount_data_volume` picks the first ext2 device that is not the root,
/// skips non-ext2 devices, and mounts nothing (without error) when there is
/// no candidate.
pub fn data_volume_probe() -> Result<(), String> {
    task::register_kernel();
    let blank = FakeDisk::new("test-data-blank", 64);
    let root_disk = pooled_disk(0);
    let first = pooled_disk(1);
    let second = pooled_disk(2);
    for disk in [root_disk, first, second] {
        format(disk);
    }
    let devices: [&'static dyn BlockDevice; 4] = [blank, root_disk, first, second];
    let root = Id::ROOT;

    let mut vfs = Vfs::new();
    vfs.mount("/", Arc::new(crate::fs::ramfs::RamFs::new()))
        .map_err(fs_error)?;
    let volume = crate::fs::mount_data_volume(&mut vfs, Some(root_disk.name()), &devices);
    check!(volume.is_some(), "no data volume was mounted");
    check!(
        vfs.mounts().iter().any(|(point, _)| point == "/data"),
        "the volume is not at /data"
    );
    vfs.create(root, "/data/x", 0o644).map_err(fs_error)?;
    check!(
        raw_state(first) & 1 == 0 && raw_state(root_disk) & 1 == 1 && raw_state(second) & 1 == 1,
        "the data volume is not the first non-root ext2 device"
    );

    let mut vfs = Vfs::new();
    check!(
        crate::fs::mount_data_volume(&mut vfs, Some(root_disk.name()), &devices[..2]).is_none()
            && vfs.mounts().is_empty(),
        "the root device (or a blank one) was mounted as data"
    );
    check!(
        crate::fs::mount_data_volume(&mut vfs, None, &[]).is_none(),
        "an empty device list mounted something"
    );
    Ok(())
}

/// `/` is the FAT boot volume whatever the enumeration order: an ext2 disk
/// listed ahead of it must not become the root, and with no FAT volume the
/// first ext2 one is used.
pub fn root_prefers_fat_over_ext2() -> Result<(), String> {
    task::register_kernel();
    let ext2_disk = pooled_disk(0);
    format(ext2_disk);
    let fat_disk = FakeDisk::new("test-root-fat", crate::tests::boot_io_suite::IMAGE_SECTORS);
    fat_disk
        .data
        .lock()
        .copy_from_slice(&crate::tests::boot_io_suite::fragmented_image());

    let ext2_first: [&'static dyn BlockDevice; 2] = [ext2_disk, fat_disk];
    let picked = crate::fs::select_root(&ext2_first).map(|(fs, name)| (fs.name(), name));
    check!(
        picked == Some(("fat16 (ro)", fat_disk.name())),
        "an ext2 disk listed first took `/` from the FAT volume: {picked:?}"
    );

    let picked = crate::fs::select_root(&ext2_first[..1]).map(|(fs, name)| (fs.name(), name));
    check!(
        picked == Some(("ext2 (rw)", ext2_disk.name())),
        "with no FAT volume the ext2 disk was not chosen: {picked:?}"
    );
    check!(
        crate::fs::select_root(&[]).is_none(),
        "an empty device list produced a root"
    );
    Ok(())
}

/// Soak: 200 generations of change, an optional sync, and a remount. The
/// on-disk state always matches what was last done (an unsynced stop is seen as
/// unclean by the next mount), and the file survives every hop.
pub fn soak_state_generations() -> Result<(), String> {
    task::register_kernel();
    let (fs, vfs, disk) = mounted(1024, 512)?;
    drop((fs, vfs));
    let root = Id::ROOT;
    let mut rng = Rng(0xC0FF_EE11);
    let mut expected = Vec::new();
    for generation in 0..200u32 {
        let (fs, mut vfs) = remount_disk(disk)?;
        check!(
            fs.was_clean_at_mount(),
            "generation {generation}: mounted unclean"
        );
        if generation > 0 {
            check!(
                vfs.read_file(root, "/f").map_err(fs_error)? == expected,
                "generation {generation}: the file changed across a remount"
            );
            vfs.unlink(root, "/f").map_err(fs_error)?;
        }
        expected = pattern_bytes(generation, 1 + rng.below(3000) as usize);
        vfs.create(root, "/f", 0o644).map_err(fs_error)?;
        vfs.write(root, "/f", 0, &expected).map_err(fs_error)?;
        let synced = rng.below(2) == 0;
        if synced {
            fs.flush().map_err(fs_error)?;
        }
        check!(
            (raw_state(disk) & 1 == 1) == synced,
            "generation {generation}: the disk state disagrees with the sync"
        );
        drop((fs, vfs)); // a stop, clean or not
        if !synced {
            let (fs, _vfs) = remount_disk(disk)?;
            check!(
                !fs.was_clean_at_mount(),
                "generation {generation}: unsynced stop unseen"
            );
            disk.data.lock()[SUPER + 0x3A] = 1; // "fsck": clean again for the next hop
        }
    }
    Ok(())
}
