//! The stick's FAT partition as the payload (decision 2): the UEFI loader,
//! the kernel, `boot.json` and the ramdisk, whose entry 1 is the FAT `/boot`
//! with `lazyos.cfg` and entry 2 the OS volume (docs/usb-stick.md).

use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ext2fs::Ext2;

use crate::os_disk::{self, FileIo, SECTOR};
use crate::usb_ramdisk::{self, FAT12_TYPE, LINUX_TYPE};

/// The stick's files, read and checked.
pub struct Payload {
    pub dir: PathBuf,
    /// `efi/boot/bootx64.efi`.
    pub loader: Vec<u8>,
    /// `kernel-x86_64`.
    pub kernel: Vec<u8>,
    /// `boot.json`, when the stick has one.
    pub boot_json: Option<Vec<u8>>,
    /// The stick's `lazyos.cfg`, from the ramdisk's FAT.
    pub cfg: String,
    /// The ramdisk file and its OS volume's extent in it.
    pub ramdisk: PathBuf,
    pub os_start: u64,
    pub os_sectors: u64,
}

/// `name` (slash-separated) under `dir`, matched without regard to case, as
/// a FAT volume mounted on Linux may present it either way.
pub fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut at = dir.to_path_buf();
    for part in name.split('/') {
        let exact = at.join(part);
        if exact.exists() {
            at = exact;
            continue;
        }
        let entry = std::fs::read_dir(&at).ok()?.flatten().find(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(part))
        })?;
        at = entry.path();
    }
    Some(at)
}

fn read(dir: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = find(dir, name).ok_or_else(|| format!("{} has no {name}", dir.display()))?;
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

impl Payload {
    /// Read the stick mounted at `dir`.
    pub fn open(dir: &Path) -> Result<Payload, String> {
        let loader = read(dir, "efi/boot/bootx64.efi")?;
        if loader.get(..2) != Some(b"MZ") {
            return Err("efi/boot/bootx64.efi is not a PE image".into());
        }
        let kernel = read(dir, "kernel-x86_64")?;
        if kernel.get(..4) != Some(b"\x7fELF") {
            return Err("kernel-x86_64 is not an ELF file".into());
        }
        let boot_json = find(dir, "boot.json")
            .map(|path| std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display())))
            .transpose()?;
        let ramdisk =
            find(dir, "ramdisk").ok_or_else(|| format!("{} has no ramdisk", dir.display()))?;
        let (cfg, os_start, os_sectors) = read_ramdisk(&ramdisk)?;
        Ok(Payload {
            dir: dir.to_path_buf(),
            loader,
            kernel,
            boot_json,
            cfg,
            ramdisk,
            os_start,
            os_sectors,
        })
    }

    /// The OS volume, read-only.
    pub fn os_volume(&self) -> Result<Arc<Ext2>, String> {
        let file =
            File::open(&self.ramdisk).map_err(|e| format!("{}: {e}", self.ramdisk.display()))?;
        let io = FileIo::new(file, self.os_start, self.os_sectors, false);
        let volume = Ext2::open(Box::new(io), crate::os_image::now)
            .map_err(|e| format!("the stick's OS volume: {e:?}"))?;
        Ok(Arc::new(volume))
    }

    /// The OS volume's size: the floor for the system partition.
    pub fn os_bytes(&self) -> u64 {
        self.os_sectors * SECTOR
    }

    /// The ESP files with their bytes, in [`crate::plan::ESP_FILES`] order;
    /// `ramdisk` is the config ramdisk the caller generated.
    pub fn esp_files(&self, config_ramdisk: Vec<u8>) -> Vec<(&'static str, Vec<u8>)> {
        let mut files = vec![("ramdisk", config_ramdisk)];
        if let Some(json) = &self.boot_json {
            files.push(("boot.json", json.clone()));
        }
        files.push((crate::plan::LOADER, self.loader.clone()));
        files.push(("kernel-x86_64", self.kernel.clone()));
        files
    }
}

/// Check the ramdisk's two MBR entries and read `lazyos.cfg` from the first.
fn read_ramdisk(path: &Path) -> Result<(String, u64, u64), String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut mbr = [0u8; 512];
    file.read_exact(&mut mbr)
        .map_err(|e| format!("the ramdisk: {e}"))?;
    let entry =
        |n| os_disk::mbr_entry(&mbr, n).filter(|&(kind, _, sectors)| kind != 0 && sectors > 0);
    let (Some((FAT12_TYPE, fat_start, fat_sectors)), Some((LINUX_TYPE, os_start, os_sectors))) =
        (entry(1), entry(2))
    else {
        return Err("the ramdisk is not a LazyOS stick ramdisk (FAT12 + ext2)".into());
    };
    if !os_disk::has_signature(&mbr) {
        return Err("the ramdisk has no MBR signature".into());
    }
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    if (os_start + os_sectors) * SECTOR > length || (fat_start + fat_sectors) * SECTOR > length {
        return Err("the ramdisk is shorter than its partitions".into());
    }
    let mut fat = vec![0u8; (fat_sectors * SECTOR) as usize];
    file.seek(SeekFrom::Start(fat_start * SECTOR))
        .and_then(|_| file.read_exact(&mut fat))
        .map_err(|e| format!("the ramdisk's FAT: {e}"))?;
    let cfg = usb_ramdisk::fat_read(Cursor::new(fat), fhs::boot::LAZYOS_CFG)?;
    let cfg = String::from_utf8(cfg).map_err(|_| "lazyos.cfg is not UTF-8".to_string())?;
    Ok((cfg, os_start, os_sectors))
}

/// The config-only ramdisk (decision 4): the stick ramdisk's layout with its
/// FAT `/boot` holding `cfg` and no OS volume.
pub fn config_ramdisk(cfg: &str) -> Result<Vec<u8>, String> {
    let fat = usb_ramdisk::fat_volume(cfg.as_bytes())?;
    let start = usb_ramdisk::FAT_START_LBA;
    let mut disk = vec![0u8; ((start + usb_ramdisk::FAT_SECTORS) * SECTOR) as usize];
    usb_ramdisk::set_entry(
        &mut disk[..512],
        1,
        FAT12_TYPE,
        start,
        usb_ramdisk::FAT_SECTORS,
    );
    disk[510] = 0x55;
    disk[511] = 0xAA;
    disk[(start * SECTOR) as usize..].copy_from_slice(&fat);
    Ok(disk)
}
