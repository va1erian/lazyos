//! What the stick image (`usb_image`) and the network payload (`netboot`)
//! both carry: the trimmed kernel, the ramdisk, the UEFI loader and the
//! loader's config. [`prepare`] makes them once, so a build with
//! `LAZYOS_USB_IMAGE=1` and `LAZYOS_NETBOOT=1` puts identical bytes in both
//! (the ramdisk's OS volume UUID is generated per build, so it is written once
//! and copied, never written twice).

use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::netboot::manifest::BuildInfo;
use crate::os_image::{new_uuid, OsFile};
use crate::os_layout::{Account, DirSpec};
use crate::{netboot, usb_image, usb_ramdisk, usb_stick};

/// The UEFI application's path on the FAT partition: the removable-media boot
/// path every UEFI firmware tries.
pub const UEFI_BOOT_PATH: &str = "efi/boot/bootx64.efi";

/// Smallest display mode the stick asks the firmware for. The loader only
/// changes mode when it is given a minimum, and then takes the last (largest)
/// listed mode that satisfies it; with none it keeps the firmware's default,
/// which is 800x600 on some PCs (the Kaby Lake box, docs/compat/kabylake): a
/// stretched desktop and boot text so large it clips. Anything above 1080p is
/// cut to a centred 1080p logical screen by the kernel (`display.rs`) unless
/// `LAZYOS_DISPLAY_MAX` lifts the cap (issue #717), so a large mode is safe.
/// Only the stick and the network payload set it: `lazyos.img` keeps the
/// default mode its QEMU tests were written against.
const MIN_FRAMEBUFFER: (u64, u64) = (1280, 720);

/// The made payload.
pub struct Payload {
    /// The trimmed kernel ELF.
    pub kernel: PathBuf,
    /// The ramdisk image (`usb_ramdisk::write`).
    pub ramdisk: PathBuf,
    pub written: usb_ramdisk::Written,
    /// The OS volume's UUID, named by the ramdisk's `lazyos.cfg`.
    pub uuid: [u8; 16],
    /// The bootloader's UEFI application (a PE image).
    pub loader: Vec<u8>,
    /// The loader's `boot.json`, byte for byte as `DiskImageBuilder` writes it.
    pub boot_json: Vec<u8>,
}

/// Re-run the build when one of the modules that make the payload changes.
pub fn track_sources() {
    for module in [
        "drivers",
        "usb_fat",
        "usb_image",
        "usb_payload",
        "usb_ramdisk",
        "usb_stick",
        "netboot",
        "netboot_manifest",
    ] {
        println!("cargo:rerun-if-changed=build_support/{module}.rs");
    }
}

/// An environment variable Cargo sets for the build script, as a path.
fn cargo_dir(var: &str) -> Result<PathBuf, String> {
    std::env::var_os(var)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{var} is not set"))
}

/// Write what the build was asked for from one made payload: the stick image
/// (`LAZYOS_USB_IMAGE=1`) and `target/netboot/` (`LAZYOS_NETBOOT=1`, with the
/// build `netboot` named for `lazyos.cfg`). Nothing when neither is asked for.
pub fn build_outputs(
    kernel: &Path,
    dirs: &[DirSpec],
    accounts: &[Account],
    files: &[OsFile],
    netboot: Option<&BuildInfo>,
) -> Result<(), String> {
    let stick = usb_image::enabled();
    if !stick && netboot.is_none() {
        return Ok(());
    }
    let (out_dir, source) = (cargo_dir("OUT_DIR")?, cargo_dir("CARGO_MANIFEST_DIR")?);
    let id = netboot.map(|info| info.id.as_str());
    let payload = prepare(kernel, &out_dir, dirs, files, id)?;
    if stick {
        let image = source.join("target").join("lazyos-usb.img");
        usb_image::build(&payload, &image, accounts)?;
    }
    if let Some(info) = netboot {
        let inputs = netboot::Inputs {
            kernel: &payload.kernel,
            ramdisk: &payload.ramdisk,
            loader: &payload.loader,
            boot_json: &payload.boot_json,
        };
        netboot::build(&source, &inputs, &payload.written, info)?;
    }
    Ok(())
}

/// Whether `var` is `1`.
fn on(var: &str) -> bool {
    std::env::var_os(var).as_deref() == Some(OsStr::new("1"))
}

/// A size from the environment (`512M`, `1G`), or `default`.
pub fn size(var: &str, default: u64, min: u64) -> Result<u64, String> {
    match std::env::var(var) {
        Ok(text) => usb_stick::parse_size(var, &text, min),
        Err(_) => Ok(default),
    }
}

/// Check the profile, then write the ramdisk (with an `image.id=` line when
/// `image_id` is given) into `out_dir` and read the loader files.
pub fn prepare(
    kernel: &Path,
    out_dir: &Path,
    dirs: &[DirSpec],
    files: &[OsFile],
    image_id: Option<&str>,
) -> Result<Payload, String> {
    println!("cargo:rerun-if-env-changed=LAZYOS_USB_ROOT_FREE");
    usb_stick::check_profile(
        on("LAZYOS_USB"),
        on("LAZYOS_SERVICES") || on("LAZYOS_DESKTOP"),
        files,
    )?;
    let settings = usb_ramdisk::Settings {
        uuid: new_uuid(),
        root_free: size("LAZYOS_USB_ROOT_FREE", usb_ramdisk::DEFAULT_ROOT_FREE, 0)?,
        image_id: image_id.map(str::to_owned),
    };
    let ramdisk = out_dir.join("usb-ramdisk.img");
    let written = usb_ramdisk::write(&ramdisk, &settings, dirs, files)?;
    let boot_json = serde_json::to_vec_pretty(&boot_config())
        .map_err(|e| format!("serialize the boot config: {e}"))?;
    Ok(Payload {
        kernel: kernel.to_path_buf(),
        ramdisk,
        written,
        uuid: settings.uuid,
        loader: uefi_loader(out_dir)?,
        boot_json,
    })
}

pub fn boot_config() -> bootloader::BootConfig {
    let mut config = bootloader::BootConfig::default();
    config.frame_buffer.minimum_framebuffer_width = Some(MIN_FRAMEBUFFER.0);
    config.frame_buffer.minimum_framebuffer_height = Some(MIN_FRAMEBUFFER.1);
    config
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
