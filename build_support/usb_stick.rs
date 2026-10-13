//! The USB stick image, `target/lazyos-usb.img` (docs/usb-stick.md): the
//! bootloader's hybrid boot part plus a persistent ext2 home partition.
//!
//! Layout (MBR, 512-byte sectors):
//!
//! | Entry | Content |
//! |---|---|
//! | 1 | `bootloader` BIOS stage 2 (type 0x20), from LBA 1 |
//! | 2 | FAT `/boot` (type 0x0C, active): `kernel-x86_64`, `ramdisk`, the BIOS stages 3/4 and `efi/boot/bootx64.efi` |
//! | 3 | ext2 home volume, label `lazyhome`, 1 MiB aligned after entry 2, [`Settings::home_size`] long; always the last partition, so a writer can grow it to fill the stick |
//!
//! Legacy BIOS boots the MBR code into stage 2; UEFI firmware treats the stick
//! as removable media, finds the FAT partition through the MBR and runs
//! `\EFI\BOOT\BOOTX64.EFI`. Either stage loads the kernel and the ramdisk
//! (`usb_ramdisk`) from that one partition. The home volume has the layout of
//! `target/home.img` (`python -m tools.mkdisk --home-volume`): one `<user>/`
//! per account whose home is `/home/<user>`, 0700, owned by that account.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use ext2fs::{Ext2, Geometry};

use crate::os_disk::{self, FileIo, SECTOR};
use crate::os_image::{self, OsFile};
use crate::os_layout::{Account, DirSpec, PRIVATE};
use crate::usb_ramdisk::{round_up, set_entry, LINUX_TYPE};

/// The home volume's label (`home=LABEL=lazyhome` in `lazyos.cfg`).
pub const HOME_LABEL: &str = "lazyhome";
/// Default home partition size (`LAZYOS_USB_HOME_SIZE`).
pub const DEFAULT_HOME_SIZE: u64 = 1 << 30;
/// Smallest home partition the build accepts.
pub const MIN_HOME_SIZE: u64 = 16 << 20;
/// Partition alignment: 1 MiB, what every partitioning tool uses.
pub const ALIGN_SECTORS: u64 = 2048;

/// The stick's knobs.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub home_size: u64,
}

/// What [`compose`] wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stick {
    pub bytes: u64,
    pub home_start_lba: u64,
    pub home_uuid: [u8; 16],
}

/// A size like `512M`, `1G` or `65536` (bytes; binary suffixes), at least
/// `min`, rounded down to 4 KiB; `var` names the variable in errors.
pub fn parse_size(var: &str, text: &str, min: u64) -> Result<u64, String> {
    let text = text.trim();
    let (digits, shift) = match text.chars().last() {
        Some('K' | 'k') => (&text[..text.len() - 1], 10),
        Some('M' | 'm') => (&text[..text.len() - 1], 20),
        Some('G' | 'g') => (&text[..text.len() - 1], 30),
        _ => (text, 0),
    };
    let count: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("{var}={text:?} is not a size like 512M or 2G"))?;
    let bytes = count
        .checked_mul(1 << shift)
        .filter(|bytes| *bytes <= u64::from(u32::MAX) * SECTOR / 2)
        .ok_or_else(|| format!("{var}={text:?} is too large"))?
        & !4095;
    if bytes < min {
        return Err(format!(
            "{var}={text:?} is below the {}M minimum",
            min >> 20
        ));
    }
    Ok(bytes)
}

/// The stick is for a real PC that may have no PS/2 port, so its image must
/// ship `usbd` (USB keyboard and mouse) and boot `init`, which starts it:
/// `LAZYOS_USB=1` plus a services session (`LAZYOS_SERVICES=1`, or the desktop
/// profile, which implies it). `files` must then carry `usbd` itself.
pub fn check_profile(usb: bool, services: bool, files: &[OsFile]) -> Result<(), String> {
    if !usb || !services {
        return Err("the stick needs USB input: build with LAZYOS_USB=1 and \
                    LAZYOS_DESKTOP=1 (or LAZYOS_SERVICES=1), as tools/boot/run.py does"
            .into());
    }
    let usbd = fhs::bin::USBD.trim_start_matches('/');
    if !files
        .iter()
        .any(|file| file.path.trim_start_matches('/') == usbd)
    {
        return Err(format!(
            "LAZYOS_USB=1 but {} is not in the file list",
            fhs::bin::USBD
        ));
    }
    Ok(())
}

/// The home volume's directories: `/<user>` for each account whose home is
/// `/home/<user>` (0700, the account's uid and gid), as `tools/mkdisk`
/// formats `target/home.img`.
pub fn home_dirs(accounts: &[Account]) -> Vec<DirSpec> {
    accounts
        .iter()
        .filter(|account| account.home == fhs::home_of(&account.name))
        .map(|account| {
            DirSpec::new(
                &format!("/{}", account.name),
                PRIVATE,
                account.uid,
                account.gid,
            )
        })
        .collect()
}

/// Where the pieces of a stick image go, from the boot part's MBR.
struct Layout {
    home_start: u64,
    home_sectors: u64,
    /// The whole image, in bytes.
    total: u64,
}

/// Check the boot part (`mbr` is its first sector, `boot_len` the bytes it
/// has) and place the home partition after it.
fn layout(mbr: &[u8], boot_len: u64, settings: &Settings) -> Result<Layout, String> {
    let (kind, start, sectors) = os_disk::mbr_entry(mbr, 2).ok_or("no MBR in the boot image")?;
    if kind == 0 || !os_disk::has_signature(mbr) {
        return Err("the boot image has no FAT partition in MBR entry 2".into());
    }
    if !matches!(os_disk::mbr_entry(mbr, 3), Some((0, _, _))) {
        return Err("MBR entry 3 of the boot image is already in use".into());
    }
    let boot_end = (start + sectors) * SECTOR;
    if boot_len < boot_end {
        return Err("the boot image is shorter than its FAT partition".into());
    }
    let home_start = round_up(start + sectors, ALIGN_SECTORS);
    let home_sectors = settings.home_size / SECTOR;
    let total = (home_start + home_sectors) * SECTOR;
    if home_start + home_sectors > u64::from(u32::MAX) {
        return Err("the stick image is past the 2 TiB MBR limit".into());
    }
    Ok(Layout {
        home_start,
        home_sectors,
        total,
    })
}

/// Make the stick image `image` from `boot`, the open file the bootloader
/// wrote its MBR, stage 2 and FAT partition into (MBR entry 1 stage 2, entry 2
/// the FAT partition), which is at `boot_path`: once `tidy` has fixed the
/// partition's directories (given the file and the partition's byte range)
/// and the home volume is formatted after it, the file is renamed to `image`.
/// Nothing of the boot part is read or written twice, and the file is not
/// opened again: opening a large file just written can wait on the host's
/// virus scanner for seconds. A failure removes `boot_path`.
pub fn compose_file(
    image: &Path,
    boot_path: &Path,
    boot: File,
    settings: &Settings,
    home: &[DirSpec],
    tidy: impl FnOnce(&File, u64, u64) -> Result<usize, String>,
) -> Result<Stick, String> {
    let result = (|| {
        let mut file = boot;
        let length = file.metadata().map_err(|e| e.to_string())?.len();
        let mut mbr = [0u8; 512];
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.read_exact(&mut mbr))
            .map_err(|e| format!("read the boot part's MBR: {e}"))?;
        let place = layout(&mbr, length, settings)?;
        let (_, fat_start, fat_sectors) =
            os_disk::mbr_entry(&mbr, 2).ok_or("no MBR in the boot image")?;
        tidy(&file, fat_start * SECTOR, fat_sectors * SECTOR)
            .map_err(|e| format!("the stick's FAT partition: {e}"))?;
        set_entry(
            &mut mbr,
            3,
            LINUX_TYPE,
            place.home_start,
            place.home_sectors,
        );
        os_disk::mark_sparse(&file);
        file.set_len(place.total).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.write_all(&mbr))
            .map_err(|e| format!("write the MBR: {e}"))?;
        let uuid = add_home(file, &place, home)?;
        Ok((place, uuid))
    })();
    finish(boot_path, image, result)
}

/// Format the home volume into `file` at `place`; returns its UUID.
fn add_home(file: File, place: &Layout, home: &[DirSpec]) -> Result<[u8; 16], String> {
    let uuid = os_image::new_uuid();
    let io = FileIo::volatile(file, place.home_start, place.home_sectors, true);
    format_home(io, uuid, home)?;
    Ok(uuid)
}

/// Rename the finished `temp` to `image`, or remove it when `made` failed.
fn finish(
    temp: &Path,
    image: &Path,
    made: Result<(Layout, [u8; 16]), String>,
) -> Result<Stick, String> {
    let (place, uuid) = match made {
        Ok(made) => made,
        Err(error) => {
            let _ = std::fs::remove_file(temp);
            return Err(error);
        }
    };
    std::fs::rename(temp, image).map_err(|e| {
        format!(
            "cannot replace {} ({e}); is QEMU or a writer using it?",
            image.display()
        )
    })?;
    Ok(Stick {
        bytes: place.total,
        home_start_lba: place.home_start,
        home_uuid: uuid,
    })
}

/// Format `io` as the home volume and create `dirs` on it.
pub fn format_home(io: FileIo, uuid: [u8; 16], dirs: &[DirSpec]) -> Result<(), String> {
    let size = ext2fs::BlockIo::sector_count(&io) * SECTOR;
    let stamp = os_image::now();
    ext2fs::format(&io, Geometry::for_size(size), HOME_LABEL, uuid, stamp)
        .map_err(|e| format!("format the home volume: {e:?}"))?;
    let volume =
        Ext2::open(Box::new(io), os_image::now).map_err(|e| format!("open home: {e:?}"))?;
    for dir in dirs {
        volume
            .mkdir_p(&dir.path, dir.mode, dir.uid, dir.gid)
            .map_err(|e| format!("mkdir {} on home: {e:?}", dir.path))?;
    }
    volume.flush().map_err(|e| format!("flush home: {e:?}"))
}
