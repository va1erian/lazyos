//! A volume made by `libs/ext2fs` itself (the formatter and populator the
//! host build will use) opens through the kernel adapter and mounts at `/`
//! from a `lazyos.cfg`, the F1 path. This is the seam the build-time image
//! relies on: whatever the library writes, the kernel must read.

use super::*;
use crate::block::BlockDevice;
use crate::fs::mounts;
use crate::fs::vfs::{FsError, Id};
use alloc::boxed::Box;

fn lib_error(error: ext2fs::Ext2Error) -> String {
    format!("ext2fs: {error:?}")
}

fn fs_error(error: FsError) -> String {
    format!("{} ({error:?})", error.message())
}

const STAMP: i64 = 1_700_000_000;

/// Format 1 MiB of `device` as the OS volume would be (4 KiB blocks), then
/// populate it the way the image composer does: directories with owners,
/// files with modes.
fn make_volume(device: &'static dyn BlockDevice, uuid: [u8; 16]) -> Result<(), String> {
    make_volume_with(device, uuid, false)
}

/// [`make_volume`], optionally giving the volume an ext2 journal.
fn make_volume_with(
    device: &'static dyn BlockDevice,
    uuid: [u8; 16],
    journal: bool,
) -> Result<(), String> {
    let geometry = ext2fs::Geometry {
        block_size: 4096,
        blocks_count: 256,
        bytes_per_inode: 16 * 1024,
    };
    ext2fs::format(&device, geometry, "lazyos-root", uuid, STAMP).map_err(lib_error)?;
    let volume = ext2fs::Ext2::open(Box::new(device), crate::fs::vfs::now).map_err(lib_error)?;
    if journal {
        volume.add_journal(64).map_err(lib_error)?;
    }
    volume
        .mkdir_p("/home/user", 0o700, 1000, 1000)
        .map_err(lib_error)?;
    volume
        .mkdir_p("/data/tmp", 0o1777, 0, 0)
        .map_err(lib_error)?;
    for dir in [fhs::SYSTEM_BIN, fhs::SYSTEM_ETC] {
        volume.mkdir_p(dir, 0o755, 0, 0).map_err(lib_error)?;
    }
    volume
        .write_file(fhs::etc::PASSWD, b"admin:x:0:0\n", 0o644, 0, 0, STAMP)
        .map_err(lib_error)?;
    let elf = [0x7F, b'E', b'L', b'F', 2, 1, 1, 0];
    volume
        .write_file(fhs::bin::INIT, &elf, 0o755, 0, 0, STAMP)
        .map_err(lib_error)?;
    volume.flush().map_err(lib_error)
}

/// The library's image mounts as `/` through the adapter, shows the modes and
/// owners it was given, takes writes through the VFS, and the library reads
/// those writes back from the same bytes after a sync.
pub fn library_formatted_root_mounts() -> Result<(), String> {
    let (uuid_bytes, uuid_text) = uuid(0xC3);
    let root = FakeDisk::new("lf-root", 2048);
    make_volume(root, uuid_bytes)?;
    let cfg = format!("root=UUID={uuid_text}\n");
    let image = image_with_file(b"LAZYOS  CFG", cfg.as_bytes());
    let boot = FakeDisk::new("lf-boot", image.len() / SECTOR_SIZE);
    boot.data.lock().copy_from_slice(&image);

    let devices: [&'static dyn BlockDevice; 2] = [boot, root];
    let mut tables = mounts::build(&devices);
    check!(tables.mounted, "no root mounted");
    let mounted = tables.native.mounts();
    check!(
        mounted
            .first()
            .is_some_and(|(point, name)| point.as_str() == "/" && name.starts_with("ext2")),
        "the library volume is not the root: {mounted:?}"
    );

    let id = Id::ROOT;
    let native = &mut tables.native;
    let home = native.stat(id, "/home/user").map_err(fs_error)?;
    check!(
        (home.uid, home.gid, home.mode & 0o7777) == (1000, 1000, 0o700),
        "user's home is {:?}",
        (home.uid, home.gid, home.mode & 0o7777)
    );
    let tmp = native.stat(id, "/data/tmp").map_err(fs_error)?;
    check!(tmp.mode & 0o7777 == 0o1777, "/data/tmp mode {:o}", tmp.mode);
    let elf = native.stat(id, fhs::bin::INIT).map_err(fs_error)?;
    check!(
        elf.mode & 0o7777 == 0o755 && elf.size == 8 && elf.times.mtime == STAMP,
        "init is {elf:?}"
    );
    let mut passwd = [0u8; 32];
    let read = native
        .read(id, fhs::etc::PASSWD, 0, &mut passwd)
        .map_err(fs_error)?;
    check!(
        &passwd[..read] == b"admin:x:0:0\n",
        "passwd reads back wrong"
    );
    let stats = native.statfs(id, "/").map_err(fs_error)?;
    check!(
        (stats.magic, stats.block_size) == (0xEF53, 4096),
        "statfs reports {stats:?}"
    );

    native
        .create(id, "/home/user/note", 0o600)
        .map_err(fs_error)?;
    native
        .write(id, "/home/user/note", 0, b"written by the kernel")
        .map_err(fs_error)?;
    native.sync_all().map_err(fs_error)?;

    let device: &'static dyn BlockDevice = root;
    let volume = ext2fs::Ext2::open(Box::new(device), crate::fs::vfs::now).map_err(lib_error)?;
    check!(
        volume.was_clean_at_mount(),
        "the sync did not leave the volume clean"
    );
    let note = volume.read_file("/home/user/note").map_err(lib_error)?;
    check!(
        note == b"written by the kernel",
        "the library read {note:?}"
    );
    // The disks are leaked (the registry wants `'static`) and the test heap is
    // small: hand their memory back so later suites keep their room.
    drop(volume);
    drop(tables);
    for disk in [boot, root] {
        *disk.data.lock() = Vec::new();
    }
    Ok(())
}

/// A journaled volume mounts through the adapter (the cache logs its commits),
/// and a power cut at every device write of a create-write-sync leaves a
/// volume the next mount replays to one of two states: the file absent, or the
/// file whole. Never a half-written one, never a refused mount.
pub fn library_journaled_root_power_cut_sweep() -> Result<(), String> {
    const NOTE: &[u8] = b"journaled by the kernel";
    let (uuid_bytes, uuid_text) = uuid(0xD4);
    let root = FakeDisk::new("lj-root", 2048);
    make_volume_with(root, uuid_bytes, true)?;
    let pristine = root.data.lock().clone();
    let cfg = format!("root=UUID={uuid_text}
");
    let image = image_with_file(b"LAZYOS  CFG", cfg.as_bytes());
    let boot = FakeDisk::new("lj-boot", image.len() / SECTOR_SIZE);
    boot.data.lock().copy_from_slice(&image);
    let devices: [&'static dyn BlockDevice; 2] = [boot, root];

    let (mut completed, mut replayed) = (false, false);
    for k in 1..400 {
        root.data.lock().copy_from_slice(&pristine);
        let mut tables = mounts::build(&devices);
        check!(tables.mounted, "cut {k}: no root mounted");
        root.cut_power_at(k);
        let native = &mut tables.native;
        let finished = native.create(Id::ROOT, "/home/user/note", 0o600).is_ok()
            && native.write(Id::ROOT, "/home/user/note", 0, NOTE).is_ok()
            && native.sync_all().is_ok();
        root.fail_nth_write(u32::MAX);
        drop(tables);

        let device: &'static dyn BlockDevice = root;
        let volume =
            ext2fs::Ext2::open(Box::new(device), crate::fs::vfs::now).map_err(lib_error)?;
        check!(volume.has_journal(), "cut {k}: the journal is gone");
        replayed |= volume.journal_recovered();
        match volume.read_file("/home/user/note") {
            Ok(note) => check!(note == NOTE || note.is_empty(), "cut {k}: the note reads {note:?}"),
            Err(ext2fs::Ext2Error::NotFound) => {
                check!(!finished, "cut {k}: a completed sync lost the file")
            }
            Err(error) => return Err(format!("cut {k}: {error:?}")),
        }
        check!(
            volume.lookup("/home/user").is_ok(),
            "cut {k}: the directory tree was damaged"
        );
        drop(volume);
        if finished {
            completed = true;
            break;
        }
    }
    check!(completed, "the sequence never completed within 400 writes");
    check!(replayed, "no cut exercised a replay");
    *root.data.lock() = Vec::new();
    *boot.data.lock() = Vec::new();
    Ok(())
}
