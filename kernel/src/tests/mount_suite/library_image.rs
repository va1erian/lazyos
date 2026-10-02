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
    let geometry = ext2fs::Geometry {
        block_size: 4096,
        blocks_count: 256,
        bytes_per_inode: 16 * 1024,
    };
    ext2fs::format(&device, geometry, "lazyos-root", uuid, STAMP).map_err(lib_error)?;
    let volume = ext2fs::Ext2::open(Box::new(device), crate::fs::vfs::now).map_err(lib_error)?;
    volume
        .mkdir_p("/data/home/alice", 0o755, 1000, 1000)
        .map_err(lib_error)?;
    volume
        .mkdir_p("/data/tmp", 0o1777, 0, 0)
        .map_err(lib_error)?;
    volume
        .write_file("/PASSWD", b"root:x:0:0\n", 0o644, 0, 0, STAMP)
        .map_err(lib_error)?;
    let elf = [0x7F, b'E', b'L', b'F', 2, 1, 1, 0];
    volume
        .write_file("/SUPER.ELF", &elf, 0o755, 0, 0, STAMP)
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
    let alice = native.stat(id, "/data/home/alice").map_err(fs_error)?;
    check!(
        (alice.uid, alice.gid, alice.mode & 0o7777) == (1000, 1000, 0o755),
        "alice's home is {:?}",
        (alice.uid, alice.gid, alice.mode & 0o7777)
    );
    let tmp = native.stat(id, "/data/tmp").map_err(fs_error)?;
    check!(tmp.mode & 0o7777 == 0o1777, "/data/tmp mode {:o}", tmp.mode);
    let elf = native.stat(id, "/SUPER.ELF").map_err(fs_error)?;
    check!(
        elf.mode & 0o7777 == 0o755 && elf.size == 8 && elf.times.mtime == STAMP,
        "SUPER.ELF is {elf:?}"
    );
    let mut passwd = [0u8; 32];
    let read = native
        .read(id, "/PASSWD", 0, &mut passwd)
        .map_err(fs_error)?;
    check!(
        &passwd[..read] == b"root:x:0:0\n",
        "PASSWD reads back wrong"
    );
    let stats = native.statfs(id, "/").map_err(fs_error)?;
    check!(
        (stats.magic, stats.block_size) == (0xEF53, 4096),
        "statfs reports {stats:?}"
    );

    native
        .create(id, "/data/home/alice/note", 0o600)
        .map_err(fs_error)?;
    native
        .write(id, "/data/home/alice/note", 0, b"written by the kernel")
        .map_err(fs_error)?;
    native.sync_all().map_err(fs_error)?;

    let device: &'static dyn BlockDevice = root;
    let volume = ext2fs::Ext2::open(Box::new(device), crate::fs::vfs::now).map_err(lib_error)?;
    check!(
        volume.was_clean_at_mount(),
        "the sync did not leave the volume clean"
    );
    let note = volume
        .read_file("/data/home/alice/note")
        .map_err(lib_error)?;
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
