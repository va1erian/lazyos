//! `fsync` flushes the device after releasing the volume's gate (issue #940):
//! on a USB stick the flush is most of an `fsync`, and a path syscall that
//! meets the gate waits for it holding its mount table. The file must still be
//! on the device before the flush and durable when `fsync` returns.

use super::*;
use crate::fs::vfs::Filesystem;
use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Flushes the hook saw during an `fsync`, and those during which the gate
/// was held. Other operations flush under the gate as they always did (a
/// commit that pending frees force inside a truncate): only `fsync` is judged.
static FLUSHES: AtomicU32 = AtomicU32::new(0);
static HELD: AtomicU32 = AtomicU32::new(0);
static IN_FSYNC: AtomicBool = AtomicBool::new(false);

/// Count every flush of `disk` an `fsync` makes, and whether `fs`'s gate was
/// held during it.
fn watch(disk: &'static FakeDisk, fs: &Arc<Ext2>) {
    FLUSHES.store(0, Ordering::Relaxed);
    HELD.store(0, Ordering::Relaxed);
    let fs = Arc::clone(fs);
    disk.set_flush_hook(Some(Box::new(move || {
        if !IN_FSYNC.load(Ordering::Relaxed) {
            return;
        }
        FLUSHES.fetch_add(1, Ordering::Relaxed);
        if !fs.gate_is_free() {
            HELD.fetch_add(1, Ordering::Relaxed);
        }
    })));
}

/// `fsync` of one file through the filesystem (the VFS's `flush_node`).
fn fsync(fs: &Arc<Ext2>, path: &str) -> Result<(), String> {
    let node = fs.open_node(path).map_err(fs_error)?.ok_or("no node")?;
    IN_FSYNC.store(true, Ordering::Relaxed);
    let result = fs.flush_node(node).map_err(fs_error);
    IN_FSYNC.store(false, Ordering::Relaxed);
    result
}

pub fn fsync_flushes_outside_the_gate() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let fs = Arc::new(Ext2::open_deferred_with_pages(disk, 256).map_err(fs_error)?);
    let mut vfs = Vfs::new();
    vfs.mount("/", fs.clone(), crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    let result = (|| {
        let data = pattern_bytes(21, 30_000);
        write_file(&mut vfs, "/f", &data)?;
        write_file(&mut vfs, "/other", &data[..5_000])?;
        watch(disk, &fs);
        fsync(&fs, "/f")?;
        check!(
            FLUSHES.load(Ordering::Relaxed) == 1,
            "fsync flushed {} times",
            FLUSHES.load(Ordering::Relaxed)
        );
        check!(
            HELD.load(Ordering::Relaxed) == 0,
            "the gate was held during the device flush"
        );
        let (_, _, mut after) = reboot_copy(disk)?;
        check!(
            read_file(&mut after, "/f")? == data,
            "the fsynced file after a reboot"
        );
        check!(fs.dirty_blocks() > 0, "fsync wrote an unrelated file");
        Ok(())
    })();
    disk.set_flush_hook(None);
    drop((fs, vfs));
    release(disk);
    release(pooled(1));
    result
}

/// Soak: fsyncs interleaved with writes to several files; every device flush
/// finds the gate free, each fsynced content survives a reboot copy, and the
/// volume checks clean at the end.
pub fn soak_fsync_outside_the_gate() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let fs = Arc::new(Ext2::open_with_pages(disk, 128).map_err(fs_error)?);
    let mut vfs = Vfs::new();
    vfs.mount("/", fs.clone(), crate::fs::vfs::MountFlags::default())
        .map_err(fs_error)?;
    let result = (|| {
        watch(disk, &fs);
        let mut synced: Vec<(String, Vec<u8>)> = Vec::new();
        for round in 0..300u32 {
            let path = format!("/k{}", round % 5);
            let data = pattern_bytes(round, 200 + (round as usize * 613) % 6_000);
            match vfs.truncate(Id::ROOT, &path, 0) {
                Ok(()) | Err(FsError::NotFound) => {}
                Err(error) => return Err(fs_error(error)),
            }
            write_file(&mut vfs, &path, &data)?;
            if round % 3 == 0 {
                fsync(&fs, &path)?;
                synced.retain(|(name, _)| *name != path);
                synced.push((path, data));
            }
            if round % 75 == 74 {
                let (_, _, mut after) = reboot_copy(disk)?;
                for (name, bytes) in &synced {
                    check!(
                        read_file(&mut after, name)? == *bytes,
                        "round {round}: {name} after a reboot"
                    );
                }
            }
        }
        check!(
            FLUSHES.load(Ordering::Relaxed) == 100,
            "{} flushes for 100 fsyncs",
            FLUSHES.load(Ordering::Relaxed)
        );
        check!(
            HELD.load(Ordering::Relaxed) == 0,
            "{} flushes with the gate held",
            HELD.load(Ordering::Relaxed)
        );
        vfs.sync_all().map_err(fs_error)?;
        check_volume(disk, BLOCKS)?;
        Ok(())
    })();
    disk.set_flush_hook(None);
    drop((fs, vfs));
    release(disk);
    release(pooled(1));
    result
}
