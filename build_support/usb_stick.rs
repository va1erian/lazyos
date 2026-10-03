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

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
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
        .map(|account| DirSpec {
            path: format!("/{}", account.name),
            mode: PRIVATE,
            uid: account.uid,
            gid: account.gid,
        })
        .collect()
}

/// Write the stick image to `image` from `boot` (the bootloader's MBR image:
/// entry 1 stage 2, entry 2 the FAT partition) and a fresh home volume. The
/// file is built next to `image` and renamed into place.
pub fn compose(
    image: &Path,
    boot: &[u8],
    settings: &Settings,
    home: &[DirSpec],
) -> Result<Stick, String> {
    let (kind, start, sectors) = os_disk::mbr_entry(boot, 2).ok_or("no MBR in the boot image")?;
    if kind == 0 || !os_disk::has_signature(boot) {
        return Err("the boot image has no FAT partition in MBR entry 2".into());
    }
    if !matches!(os_disk::mbr_entry(boot, 3), Some((0, _, _))) {
        return Err("MBR entry 3 of the boot image is already in use".into());
    }
    let boot_end = (start + sectors) * SECTOR;
    if (boot.len() as u64) < boot_end {
        return Err("the boot image is shorter than its FAT partition".into());
    }
    let home_start = round_up(start + sectors, ALIGN_SECTORS);
    let home_sectors = settings.home_size / SECTOR;
    let total = (home_start + home_sectors) * SECTOR;
    if home_start + home_sectors > u64::from(u32::MAX) {
        return Err("the stick image is past the 2 TiB MBR limit".into());
    }
    let mut head = boot[..boot_end as usize].to_vec();
    set_entry(&mut head[..512], 3, LINUX_TYPE, home_start, home_sectors);

    let temp = image.with_extension("img.tmp");
    let result = (|| {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)
            .map_err(|e| format!("create {}: {e}", temp.display()))?;
        file.set_len(total).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.write_all(&head))
            .map_err(|e| format!("write the boot part: {e}"))?;
        let uuid = os_image::new_uuid();
        format_home(
            FileIo::new(file, home_start, home_sectors, true),
            uuid,
            home,
        )?;
        Ok(uuid)
    })();
    let uuid = match result {
        Ok(uuid) => uuid,
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
    };
    std::fs::rename(&temp, image).map_err(|e| {
        format!(
            "cannot replace {} ({e}); is QEMU or a writer using it?",
            image.display()
        )
    })?;
    Ok(Stick {
        bytes: total,
        home_start_lba: home_start,
        home_uuid: uuid,
    })
}

/// Format `io` as the home volume and create `dirs` on it.
fn format_home(io: FileIo, uuid: [u8; 16], dirs: &[DirSpec]) -> Result<(), String> {
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
