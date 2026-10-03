//! `LAZYOS_USB_IMAGE=1`: also write `target/lazyos-usb.img`, the image for a
//! real PC's USB stick (docs/usb-stick.md), from the same kernel and file list
//! as `target/lazyos.img`, which stays unchanged.
//!
//! One MBR image boots both ways: the `bootloader` BIOS image (MBR code,
//! stage 2, a FAT partition) whose FAT partition also carries the bootloader's
//! UEFI application at `efi/boot/bootx64.efi`. The kernel and the ramdisk
//! (`usb_ramdisk`: `lazyos.cfg` and the whole OS volume) sit next to it, so
//! both stages load them from the same partition. `usb_stick` appends the
//! persistent home partition.
//!
//! Opt-in because it costs a second copy of the OS files on every build (the
//! numbers are in docs/usb-stick.md). `LAZYOS_USB_HOME_SIZE` (default `1G`)
//! sizes the home partition and `LAZYOS_USB_ROOT_FREE` (default `64M`) the
//! free space on the RAM root.

use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::os_disk::{self, SECTOR};
use crate::os_image::{format_uuid, new_uuid, OsFile};
use crate::os_layout::{Account, DirSpec};
use crate::{usb_fat, usb_ramdisk, usb_stick};

/// The UEFI application's path on the FAT partition: the removable-media boot
/// path every UEFI firmware tries.
const UEFI_BOOT_PATH: &str = "efi/boot/bootx64.efi";

/// Whether this build writes the stick image.
pub fn enabled() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_USB_IMAGE");
    println!("cargo:rerun-if-env-changed=LAZYOS_USB_HOME_SIZE");
    println!("cargo:rerun-if-env-changed=LAZYOS_USB_ROOT_FREE");
    std::env::var_os("LAZYOS_USB_IMAGE").as_deref() == Some(OsStr::new("1"))
}

/// Build `image` from `kernel` (the trimmed ELF), the OS directories and
/// files, and the accounts (for the home volume). Prints one summary warning.
pub fn build(
    kernel: &Path,
    out_dir: &Path,
    image: &Path,
    dirs: &[DirSpec],
    files: &[OsFile],
    accounts: &[Account],
) -> Result<(), String> {
    let started = Instant::now();
    let on = |var: &str| std::env::var_os(var).as_deref() == Some(OsStr::new("1"));
    usb_stick::check_profile(
        on("LAZYOS_USB"),
        on("LAZYOS_SERVICES") || on("LAZYOS_DESKTOP"),
        files,
    )?;
    let size = |var: &str, default: u64, min: u64| match std::env::var(var) {
        Ok(text) => usb_stick::parse_size(var, &text, min),
        Err(_) => Ok(default),
    };
    let ramdisk_settings = usb_ramdisk::Settings {
        uuid: new_uuid(),
        root_free: size("LAZYOS_USB_ROOT_FREE", usb_ramdisk::DEFAULT_ROOT_FREE, 0)?,
    };
    let stick_settings = usb_stick::Settings {
        home_size: size(
            "LAZYOS_USB_HOME_SIZE",
            usb_stick::DEFAULT_HOME_SIZE,
            usb_stick::MIN_HOME_SIZE,
        )?,
    };

    let ramdisk = out_dir.join("usb-ramdisk.img");
    let written = usb_ramdisk::write(&ramdisk, &ramdisk_settings, dirs, files)?;

    let mut builder = bootloader::DiskImageBuilder::new(kernel.to_path_buf());
    builder.set_ramdisk(ramdisk.clone());
    builder.set_file_contents(UEFI_BOOT_PATH.into(), uefi_loader(out_dir)?);
    let boot_image = out_dir.join("usb-boot.img");
    builder
        .create_bios_image(&boot_image)
        .map_err(|e| format!("the stick's boot part: {e:#}"))?;
    let mut boot = std::fs::read(&boot_image).map_err(|e| e.to_string())?;
    let (_, start, sectors) = os_disk::mbr_entry(&boot, 2).ok_or("the boot part has no MBR")?;
    let fat = boot
        .get_mut((start * SECTOR) as usize..((start + sectors) * SECTOR) as usize)
        .ok_or("the boot part is shorter than its FAT partition")?;
    usb_fat::tidy(fat).map_err(|e| format!("the stick's FAT partition: {e}"))?;
    let home = usb_stick::home_dirs(accounts);
    let stick = usb_stick::compose(image, &boot, &stick_settings, &home)?;
    let _ = std::fs::remove_file(&boot_image);

    println!(
        "cargo:warning=USB stick image {}: ramdisk {} MiB (RAM root {} MiB, {}), \
         home {} MiB at LBA {} ({}), {} MiB total, built in {:.1}s",
        image.display(),
        written.bytes >> 20,
        written.os_bytes >> 20,
        format_uuid(ramdisk_settings.uuid),
        stick_settings.home_size >> 20,
        stick.home_start_lba,
        format_uuid(stick.home_uuid),
        stick.bytes >> 20,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// The bootloader's UEFI application. `bootloader` 0.11 embeds it but exposes
/// it only inside a FAT partition it formats, so format a tiny one (no kernel,
/// no ramdisk) and read the file back out of it.
fn uefi_loader(out_dir: &Path) -> Result<Vec<u8>, String> {
    let partition: PathBuf = out_dir.join("usb-uefi-loader.fat");
    bootloader::DiskImageBuilder::empty()
        .create_uefi_fat_partition(&partition)
        .map_err(|e| format!("the UEFI loader partition: {e:#}"))?;
    let file = File::options()
        .read(true)
        .write(true)
        .open(&partition)
        .map_err(|e| e.to_string())?;
    let bytes = usb_ramdisk::fat_read(file, UEFI_BOOT_PATH)?;
    let _ = std::fs::remove_file(&partition);
    // A PE image starts with "MZ"; anything else means the crate changed shape.
    if bytes.get(..2) != Some(b"MZ") {
        return Err(format!(
            "{UEFI_BOOT_PATH} in the bootloader is not a PE image"
        ));
    }
    Ok(bytes)
}
