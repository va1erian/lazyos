//! The two ways [`compose`](super::compose) writes the image file: a new
//! image formatted beside the old one (renamed into place by the caller), or
//! an update in place under a lock, with the boot area rewritten only once the
//! volume passed `recover`.

use std::fs::OpenOptions;
use std::path::Path;

use ext2fs::Geometry;

use super::{
    ensure_journal, now, open_cached, volume_error, write_volume, OsFile, Settings, RESET_HINT,
};
use crate::os_disk::{self, FileIo, OS_START_LBA, SECTOR};
use crate::os_layout::DirSpec;
use crate::os_manifest::Manifest;
use crate::os_recover;

pub(super) fn create(
    temp: &Path,
    head: &[u8],
    uuid: [u8; 16],
    settings: &Settings,
    dirs: &[DirSpec],
    files: &[OsFile],
) -> Result<(), String> {
    let total = OS_START_LBA * SECTOR + settings.os_size;
    let sectors = settings.os_size / SECTOR;
    let journal = settings.journal;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(temp)
        .map_err(|e| format!("create {}: {e}", temp.display()))?;
    file.set_len(total).map_err(|e| e.to_string())?;
    write_head(&mut file, head, 0)?;
    let io = FileIo::new(file, OS_START_LBA, sectors, true);
    let geometry = Geometry::for_size(sectors * SECTOR);
    ext2fs::format(&io, geometry, "lazyos", uuid, now()).map_err(|e| volume_error("format", e))?;
    let volume = open_cached(io)?;
    ensure_journal(&volume, journal)?;
    write_volume(&volume, None, dirs, files, now())?;
    Ok(())
}

pub(super) fn update(
    image: &Path,
    head: &[u8],
    old: &Manifest,
    settings: &Settings,
    dirs: &[DirSpec],
    files: &[OsFile],
) -> Result<(), String> {
    let total = OS_START_LBA * SECTOR + settings.os_size;
    let sectors = settings.os_size / SECTOR;
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 1); // others may only read
    let mut file = options.open(image).map_err(|e| {
        format!(
            "cannot update {} in place ({e}); is QEMU running on it? Stop it and retry",
            image.display()
        )
    })?;
    if let Err(error) = file.try_lock() {
        return Err(format!(
            "{} is locked ({error:?}); stop whatever is using it and retry",
            image.display()
        ));
    }
    if file.metadata().map_err(|e| e.to_string())?.len() != total {
        return Err(format!("{} changed size; {RESET_HINT}", image.display()));
    }
    let old_end = old_fat_end(&mut file)?;
    // The volume goes through a duplicate of the locked handle (it shares the
    // lock), so it is checked before the boot area is touched: a refused
    // volume leaves the whole image as it was.
    let volume_file = file.try_clone().map_err(|e| e.to_string())?;
    let io = FileIo::new(volume_file, OS_START_LBA, sectors, true);
    let mut volume = open_cached(io)?;
    // `recover` commits its orphan reclaim through the cache before the
    // checker reads the raw volume.
    os_recover::recover(&mut volume, settings.update_damaged)?;
    ensure_journal(&volume, settings.journal)?;
    write_head(&mut file, head, old_end)?;
    write_volume(&volume, Some(old), dirs, files, now())?;
    Ok(())
}

/// Byte offset where the image's current FAT volume ends (MBR entry 2).
fn old_fat_end(file: &mut std::fs::File) -> Result<u64, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut mbr = [0u8; 512];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut mbr))
        .map_err(|e| format!("read the MBR: {e}"))?;
    Ok(os_disk::mbr_entry(&mbr, 2).map_or(0, |(_, start, sectors)| (start + sectors) * SECTOR))
}

/// Write the BIOS part over LBA 0 and zero up to `zero_to` bytes (the end of
/// the previous FAT volume, never past the OS volume), so a smaller `/boot`
/// leaves no stale bytes behind.
fn write_head(file: &mut std::fs::File, head: &[u8], zero_to: u64) -> Result<(), String> {
    use std::io::{Seek, SeekFrom, Write};
    let limit = (OS_START_LBA * SECTOR) as usize;
    let mut padded = head.to_vec();
    padded.resize(head.len().max((zero_to as usize).min(limit)), 0);
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    file.write_all(&padded)
        .and_then(|()| file.flush())
        .map_err(|e| format!("write the boot area: {e}"))
}
