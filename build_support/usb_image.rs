//! `LAZYOS_USB_IMAGE=1`: also write `target/lazyos-usb.img`, the image for a
//! real PC's USB stick (docs/usb-stick.md), from the same kernel and file list
//! as `target/lazyos.img`, which stays unchanged.
//!
//! One MBR image boots both ways: the `bootloader` BIOS image (MBR code,
//! stage 2, a FAT partition) whose FAT partition also carries the bootloader's
//! UEFI application at `efi/boot/bootx64.efi`. The kernel and the ramdisk
//! (`usb_ramdisk`: `lazyos.cfg` and the whole OS volume) sit next to it, so
//! both stages load them from the same partition. `usb_stick` appends the
//! persistent home partition. The kernel, ramdisk, loader and loader config
//! come from `usb_payload`, which `LAZYOS_NETBOOT=1` (`netboot`) shares.
//!
//! Opt-in because it costs a second copy of the OS files on every build (the
//! numbers are in docs/usb-stick.md). `LAZYOS_USB_HOME_SIZE` (default `1G`)
//! sizes the home partition and `LAZYOS_USB_ROOT_FREE` (default `64M`) the
//! free space on the RAM root. `LAZYOS_INSTALLER=1` also puts
//! `lazyos-install` on the FAT partition (docs/grub-install-plan.md I2).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::os_disk::{self, SECTOR};
use crate::os_image::format_uuid;
use crate::os_layout::Account;
use crate::usb_payload::{self, Payload};
use crate::{usb_fat, usb_stick};

/// Whether this build writes the stick image.
pub fn enabled() -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_USB_IMAGE");
    println!("cargo:rerun-if-env-changed=LAZYOS_USB_HOME_SIZE");
    println!("cargo:rerun-if-env-changed=LAZYOS_USB_ROOT_FREE");
    println!("cargo:rerun-if-env-changed=LAZYOS_INSTALLER");
    std::env::var_os("LAZYOS_USB_IMAGE").as_deref() == Some(OsStr::new("1"))
}

/// Build `image` from the made `payload` and the accounts (for the home
/// volume). Prints one summary warning.
pub fn build(
    payload: &Payload,
    out_dir: &Path,
    image: &Path,
    accounts: &[Account],
) -> Result<(), String> {
    let started = Instant::now();
    let stick_settings = usb_stick::Settings {
        home_size: usb_payload::size(
            "LAZYOS_USB_HOME_SIZE",
            usb_stick::DEFAULT_HOME_SIZE,
            usb_stick::MIN_HOME_SIZE,
        )?,
    };

    let mut builder = bootloader::DiskImageBuilder::new(payload.kernel.clone());
    builder.set_ramdisk(payload.ramdisk.clone());
    builder.set_boot_config(&usb_payload::boot_config());
    builder.set_file_contents(usb_payload::UEFI_BOOT_PATH.into(), payload.loader.clone());
    for (path, bytes) in installer_files()? {
        builder.set_file_contents(path, bytes);
    }
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
        payload.written.bytes >> 20,
        payload.written.os_bytes >> 20,
        format_uuid(payload.uuid),
        stick_settings.home_size >> 20,
        stick.home_start_lba,
        format_uuid(stick.home_uuid),
        stick.bytes >> 20,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// `LAZYOS_INSTALLER=1`: `install/` on the stick's FAT partition, with
/// `lazyos-install` (built by `tools/install/build.py`), the GRUB entry
/// template it writes and a README, so the stick can install itself beside
/// Linux (docs/grub-install-plan.md). Nothing without the switch.
fn installer_files() -> Result<Vec<(String, Vec<u8>)>, String> {
    if std::env::var_os("LAZYOS_INSTALLER").as_deref() != Some(OsStr::new("1")) {
        return Ok(Vec::new());
    }
    let root =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").ok_or("no CARGO_MANIFEST_DIR")?);
    let sources = [
        (
            "install/lazyos-install",
            root.join("target/install/lazyos-install"),
        ),
        (
            "install/40_lazyos.in",
            root.join("tools/install/40_lazyos.in"),
        ),
        ("install/README.txt", root.join("tools/install/README.txt")),
    ];
    let mut files = Vec::new();
    for (path, source) in sources {
        println!("cargo:rerun-if-changed={}", source.display());
        let bytes = std::fs::read(&source).map_err(|e| {
            format!(
                "LAZYOS_INSTALLER=1 needs {} ({e}); run `python tools/install/build.py`",
                source.display()
            )
        })?;
        files.push((path.to_string(), bytes));
    }
    Ok(files)
}
