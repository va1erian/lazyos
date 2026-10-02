//! The USB stick's ramdisk (docs/usb-stick.md): a small whole-disk image the
//! bootloader loads into RAM next to the kernel, so the kernel needs no driver
//! for the medium it booted from.
//!
//! Layout (512-byte sectors, MBR):
//!
//! | Entry | Start | Size | Content |
//! |---|---|---|---|
//! | 1 | LBA 2048 (1 MiB) | 1 MiB | FAT12 `/boot`: `lazyos.cfg` only |
//! | 2 | LBA 4096 (2 MiB) | the OS files plus [`Settings::root_free`] | ext2 OS volume (label `lazyos`) |
//!
//! The kernel registers the ramdisk as `ram0`, scans its MBR (`ram0p1`,
//! `ram0p2`) and mounts it through the same configured layout as a disk:
//! `/boot` from the FAT volume, `/` from the ext2 volume named by
//! `root=UUID=` in `lazyos.cfg`. The ramdisk is writable, so `/` is a RAM
//! root: changes last until power-off. The ext2 volume is written by
//! `libs/ext2fs` through [`write_volume`], exactly like the disk image's OS
//! volume, and sized to its contents so loading it over firmware USB stays
//! short.

use std::fs::OpenOptions;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

use ext2fs::{Ext2, Geometry};

use crate::os_disk::{FileIo, SECTOR};
use crate::os_image::{self, write_volume, OsFile, Source};
use crate::os_layout::DirSpec;

/// Where the FAT `/boot` partition starts, and its size, in sectors.
pub const FAT_START_LBA: u64 = 2048;
pub const FAT_SECTORS: u64 = 2048;
/// Where the ext2 OS partition starts.
pub const OS_START_LBA: u64 = FAT_START_LBA + FAT_SECTORS;
/// MBR type of the FAT12 `/boot` partition.
pub const FAT12_TYPE: u8 = 0x01;
/// MBR type of a Linux filesystem.
pub const LINUX_TYPE: u8 = 0x83;
/// Default free space left on the RAM root (`LAZYOS_USB_ROOT_FREE`).
pub const DEFAULT_ROOT_FREE: u64 = 64 << 20;
/// How often a too-small volume is regrown before the build gives up.
const SIZE_ATTEMPTS: u32 = 4;

/// What the ramdisk carries besides the files.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// The OS volume's UUID, named by `lazyos.cfg`.
    pub uuid: [u8; 16],
    /// Free bytes to leave on the ext2 volume after the files are written.
    pub root_free: u64,
}

/// What [`write`] produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written {
    /// The whole ramdisk file.
    pub bytes: u64,
    /// The ext2 OS partition.
    pub os_bytes: u64,
}

/// Write the ramdisk image to `path` (replacing it).
pub fn write(
    path: &Path,
    settings: &Settings,
    dirs: &[DirSpec],
    files: &[OsFile],
) -> Result<Written, String> {
    let fat = fat_volume(os_image::boot_cfg(settings.uuid).as_bytes())?;
    let mut os_bytes = estimate_os_bytes(dirs, files)? + settings.root_free;
    for _ in 0..SIZE_ATTEMPTS {
        match write_once(path, settings, &fat, os_bytes, dirs, files) {
            Ok(written) => return Ok(written),
            Err(Grow::NoSpace) => os_bytes = round_up(os_bytes + os_bytes / 4, 1 << 20),
            Err(Grow::Failed(error)) => return Err(error),
        }
    }
    Err(format!(
        "the RAM root does not fit in {} MiB",
        os_bytes >> 20
    ))
}

enum Grow {
    NoSpace,
    Failed(String),
}

fn write_once(
    path: &Path,
    settings: &Settings,
    fat: &[u8],
    os_bytes: u64,
    dirs: &[DirSpec],
    files: &[OsFile],
) -> Result<Written, Grow> {
    let failed = |e: String| Grow::Failed(format!("{}: {e}", path.display()));
    let os_sectors = os_bytes / SECTOR;
    let total = (OS_START_LBA + os_sectors) * SECTOR;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| failed(e.to_string()))?;
    file.set_len(total).map_err(|e| failed(e.to_string()))?;
    let mut mbr = [0u8; 512];
    set_entry(&mut mbr, 1, FAT12_TYPE, FAT_START_LBA, FAT_SECTORS);
    set_entry(&mut mbr, 2, LINUX_TYPE, OS_START_LBA, os_sectors);
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    let head = [(0, &mbr[..]), (FAT_START_LBA * SECTOR, fat)];
    for (at, bytes) in head {
        file.seek(SeekFrom::Start(at))
            .and_then(|_| file.write_all(bytes))
            .map_err(|e| failed(e.to_string()))?;
    }
    let io = FileIo::new(file, OS_START_LBA, os_sectors, true);
    let stamp = os_image::now();
    ext2fs::format(
        &io,
        Geometry::for_size(os_bytes),
        "lazyos",
        settings.uuid,
        stamp,
    )
    .map_err(|e| failed(format!("format: {e:?}")))?;
    let volume =
        Ext2::open(Box::new(io), os_image::now).map_err(|e| failed(format!("open: {e:?}")))?;
    match write_volume(&volume, None, dirs, files, stamp) {
        Ok(_) => Ok(Written {
            bytes: total,
            os_bytes,
        }),
        Err(error) if error.contains("NoSpace") => Err(Grow::NoSpace),
        Err(error) => Err(failed(error)),
    }
}

/// A first guess at the ext2 volume the files need: each file rounded up to
/// 4 KiB blocks plus its indirect blocks (one per 1024 data blocks), a block
/// per directory, the manifest, and the formatter's fixed costs (the inode
/// table is one 128-byte inode per 16 KiB, under 1 %; group metadata and
/// `lost+found`). [`write`] grows the volume if the guess is short.
pub fn estimate_os_bytes(dirs: &[DirSpec], files: &[OsFile]) -> Result<u64, String> {
    const BLOCK: u64 = 4096;
    let mut data = 0u64;
    for file in files {
        let len = match &file.source {
            Source::Bytes(bytes) => bytes.len() as u64,
            Source::Path(path) => std::fs::metadata(path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .len(),
        };
        let blocks = len.div_ceil(BLOCK);
        data += (blocks + blocks.div_ceil(1024) + 1) * BLOCK;
    }
    // Directories (and the parents `mkdir_p` adds for files) and the manifest.
    data += (dirs.len() as u64 + files.len() as u64 / 8 + 16) * BLOCK;
    let overhead = data / 50 + (4 << 20);
    Ok(round_up(data + overhead, 1 << 20))
}

/// A 1 MiB FAT12 volume (label `LAZYBOOT`) holding `lazyos.cfg` = `cfg`.
pub fn fat_volume(cfg: &[u8]) -> Result<Vec<u8>, String> {
    let mut disk = Cursor::new(vec![0u8; (FAT_SECTORS * SECTOR) as usize]);
    let options = fatfs::FormatVolumeOptions::new()
        .fat_type(fatfs::FatType::Fat12)
        .volume_label(*b"LAZYBOOT   ");
    fatfs::format_volume(&mut disk, options).map_err(|e| format!("format FAT: {e}"))?;
    {
        let fs = fatfs::FileSystem::new(&mut disk, fatfs::FsOptions::new())
            .map_err(|e| format!("open FAT: {e}"))?;
        let mut file = fs
            .root_dir()
            .create_file(fhs::boot::LAZYOS_CFG)
            .map_err(|e| format!("create {}: {e}", fhs::boot::LAZYOS_CFG))?;
        file.truncate().map_err(|e| e.to_string())?;
        file.write_all(cfg).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;
    }
    Ok(disk.into_inner())
}

/// Read `name` from a FAT volume image (tests, and the UEFI loader extraction
/// in `usb_image`).
pub fn fat_read(volume: impl Read + Write + Seek, name: &str) -> Result<Vec<u8>, String> {
    let fs = fatfs::FileSystem::new(volume, fatfs::FsOptions::new())
        .map_err(|e| format!("open FAT: {e}"))?;
    let mut file = fs
        .root_dir()
        .open_file(name)
        .map_err(|e| format!("{name}: {e}"))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// Write MBR entry `n` (1 to 4) into `mbr`.
pub fn set_entry(mbr: &mut [u8], n: usize, kind: u8, lba: u64, sectors: u64) {
    let at = 0x1BE + (n - 1) * 16;
    let entry = &mut mbr[at..at + 16];
    entry.fill(0);
    entry[4] = kind;
    entry[8..12].copy_from_slice(&(lba as u32).to_le_bytes());
    entry[12..16].copy_from_slice(&(sectors as u32).to_le_bytes());
}

/// `value` rounded up to a multiple of `unit`.
pub fn round_up(value: u64, unit: u64) -> u64 {
    value.div_ceil(unit) * unit
}
