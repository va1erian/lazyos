//! Which volumes become which mounts (docs/filesystem-plan.md F1).
//!
//! Two layouts, chosen at boot:
//!
//! * **Configured**: the FAT boot volume carries `lazyos.cfg` naming an ext2
//!   root by UUID. `/` is that volume, FAT moves to `/boot` (read-only), one
//!   ramfs serves `/transient` and `/tmp`, and an optional home volume mounts
//!   at `/home`.
//! * **Legacy**: FAT at `/`, ramfs at `/tmp`, ext2 data disk at `/data`. This
//!   is also the recovery boot: no config, or a root that is not found, lands
//!   here exactly as before.
//!
//! Selection looks at whole disks only in the legacy layout; the configured one
//! also finds the root among the MBR partitions ([`crate::block::partition`]).

use alloc::sync::Arc;

use super::bootcfg::{self, BootCfg, VolumeId};
use super::vfs::{Filesystem, MountFlags, Vfs};
use super::{ext2, fat, overlay, ramfs};
use crate::block::BlockDevice;

type Devices<'a> = &'a [&'static dyn BlockDevice];

/// Where the durable ext2 data volume lives in the legacy layout.
pub(super) const DATA_MOUNT: &str = fhs::mount::DATA;

/// The first FAT12/16 volume on any whole disk.
fn find_fat(devices: Devices) -> Option<(Arc<dyn Filesystem>, &'static str)> {
    devices
        .iter()
        .filter(|device| !device.is_partition())
        .find_map(|device| {
            let volume = fat::Fat16::open(*device)?;
            Some((Arc::new(volume) as Arc<dyn Filesystem>, device.name()))
        })
}

/// Pick the volume that becomes `/` in the legacy layout: the first FAT12/16
/// volume on any device, and only when there is none, the first ext2 one
/// (issue #99). FAT is the shipped boot format, so it wins regardless of
/// enumeration order; otherwise an ext2 data disk enumerated ahead of the boot
/// disk would take `/` and leave the boot volume unmounted.
pub(crate) fn select_root(devices: Devices) -> Option<(Arc<dyn Filesystem>, &'static str)> {
    find_fat(devices).or_else(|| {
        devices
            .iter()
            .filter(|device| !device.is_partition())
            .find_map(|device| {
                let volume = ext2::Ext2::open(*device).ok()?;
                Some((Arc::new(volume) as Arc<dyn Filesystem>, device.name()))
            })
    })
}

/// The first ext2 volume on any device (not `skip`) that satisfies `wanted`.
fn find_ext2(
    devices: Devices,
    skip: Option<&str>,
    wanted: impl Fn(&ext2::Ext2) -> bool,
) -> Option<(ext2::Ext2, &'static dyn BlockDevice)> {
    devices
        .iter()
        .filter(|device| Some(device.name()) != skip)
        .find_map(|device| {
            let volume = ext2::Ext2::open(*device).ok()?;
            wanted(&volume).then_some((volume, *device))
        })
}

/// Delete the orphaned `.unlinked-*` files an unclean stop left on `volume`
/// (it does nothing on a cleanly unmounted one) and say how many there were.
pub(super) fn reclaim_orphans(volume: &ext2::Ext2, point: &str) {
    let reclaimed = volume.reclaim_orphans();
    if reclaimed > 0 {
        serial_println!("fs: {point}: reclaimed {reclaimed} orphaned files");
    }
}

/// Mount the first ext2 volume that is not the root device at `/data`.
///
/// This is the one place the data volume is mounted (there is no mount
/// syscall). A missing device is not an error: a session without a data disk
/// simply has no `/data`. Every volume is opened at most once, and a device
/// that fails to open as ext2 is skipped, never guessed at. Returns the
/// mounted volume so the caller can share it with the Linux ABI table.
pub(crate) fn mount_data_volume(
    vfs: &mut Vfs,
    root_device: Option<&str>,
    devices: Devices,
) -> Option<Arc<dyn Filesystem>> {
    let whole: alloc::vec::Vec<_> = devices
        .iter()
        .copied()
        .filter(|device| !device.is_partition())
        .collect();
    let (volume, device) = find_ext2(&whole, root_device, |_| true)?;
    // Before the volume is visible: finish what an unclean stop left.
    reclaim_orphans(&volume, DATA_MOUNT);
    let volume: Arc<dyn Filesystem> = Arc::new(volume);
    vfs.mount(DATA_MOUNT, Arc::clone(&volume), MountFlags::default())
        .ok()?;
    serial_println!("fs: mounted {} at {DATA_MOUNT}", device.name());
    if !device.is_writable() {
        serial_println!("fs: {DATA_MOUNT} is read-only (device cannot be written)");
    }
    Some(volume)
}

/// The native and Linux ABI tables, and whether a root volume mounted.
pub(crate) struct Tables {
    pub(crate) native: Vfs,
    pub(crate) abi: Vfs,
    pub(crate) mounted: bool,
}

/// Build both tables from the registered block devices.
pub(crate) fn build(devices: Devices) -> Tables {
    if let Some((boot, boot_device)) = find_fat(devices) {
        if let Some(cfg) = bootcfg::load(&*boot) {
            if let Some(tables) = configured(&cfg, boot, boot_device, devices) {
                return tables;
            }
        }
    }
    legacy(devices)
}

/// The configured layout, or `None` (after logging) when its root is missing.
fn configured(
    cfg: &BootCfg,
    boot: Arc<dyn Filesystem>,
    boot_device: &'static str,
    devices: Devices,
) -> Option<Tables> {
    let uuid = cfg.root?;
    let Some((root, root_device)) = find_ext2(devices, Some(boot_device), |v| v.uuid() == uuid)
    else {
        serial_println!(
            "fs: root {} not found; booting the legacy layout",
            fmt_uuid(&uuid)
        );
        return None;
    };
    reclaim_orphans(&root, fhs::mount::ROOT);
    let mut root_flags = cfg.root_flags;
    if !root_device.is_writable() {
        serial_println!("fs: / is read-only (device cannot be written)");
        root_flags.ro = true;
    }
    let root: Arc<dyn Filesystem> = Arc::new(root);
    let scratch: Arc<dyn Filesystem> = Arc::new(ramfs::RamFs::scratch());
    let boot_flags = MountFlags {
        ro: true,
        noexec: true,
        nosuid: true,
    };
    let mut mounts: alloc::vec::Vec<(&str, Arc<dyn Filesystem>, MountFlags)> = alloc::vec![
        (fhs::mount::ROOT, root, root_flags),
        (fhs::mount::BOOT, boot, boot_flags),
        (
            fhs::mount::TRANSIENT,
            Arc::clone(&scratch),
            MountFlags::default()
        ),
        (fhs::mount::TMP, scratch, MountFlags::default()),
    ];
    if let Some(home) = cfg.home {
        match find_home(&home, root_device.name(), devices) {
            Some(volume) => {
                reclaim_orphans(&volume, fhs::mount::HOME);
                let flags = cfg.home_flags.union(MountFlags {
                    nosuid: true,
                    ..Default::default()
                });
                mounts.push((fhs::mount::HOME, Arc::new(volume), flags));
            }
            None => serial_println!(
                "fs: home volume {} not found; /home is a directory on /",
                describe(&home)
            ),
        }
    }
    // One set of volume instances serves both tables: the ABI sees the same
    // files with no overlay, and every mount keeps the same flags.
    let mut native = Vfs::new();
    let mut abi = Vfs::new();
    for (point, volume, flags) in mounts {
        let _ = native.mount(point, Arc::clone(&volume), flags);
        let _ = abi.mount(point, volume, flags);
    }
    log_mounts(&native, &abi);
    Some(Tables {
        native,
        abi,
        mounted: true,
    })
}

/// The home volume named by `id`, never the root's own device.
fn find_home(id: &VolumeId, root_device: &str, devices: Devices) -> Option<ext2::Ext2> {
    find_ext2(devices, Some(root_device), |v| match id {
        VolumeId::Uuid(uuid) => v.uuid() == *uuid,
        VolumeId::Label(label) => v.label() == *label,
    })
    .map(|(volume, _)| volume)
}

/// FAT (or ext2) at `/`, ramfs at `/tmp`, an ext2 data disk at `/data`; the
/// Linux ABI sees `/` as a copy-up overlay.
fn legacy(devices: Devices) -> Tables {
    let mut native = Vfs::new();
    let (root, root_device) = match select_root(devices) {
        Some((volume, name)) => (Some(volume), Some(name)),
        None => (None, None),
    };
    let plain = MountFlags::default();
    if let Some(volume) = &root {
        let _ = native.mount(fhs::mount::ROOT, Arc::clone(volume), plain);
    } else {
        serial_println!("fs: no FAT or ext2 volume on any block device");
    }
    // `/tmp` is the scratch filesystem: writable, in memory, and discarded on
    // reboot. Mounting it even when FAT is missing keeps the VFS usable. The
    // ABI table below mounts the same instance so both views agree.
    let tmp: Arc<dyn Filesystem> = Arc::new(ramfs::RamFs::new());
    let _ = native.mount(fhs::mount::TMP, Arc::clone(&tmp), plain);
    for (point, name) in native.mounts() {
        serial_println!("fs: mounted {name} at {point}");
    }
    let data = mount_data_volume(&mut native, root_device, devices);

    // The Linux ABI sees a writable root: a copy-up overlay over the read-only
    // boot volume. Upper-layer contents live in memory and are discarded on
    // reboot; native tasks keep the raw mounts above.
    let mounted = root.is_some();
    let abi_root: Arc<dyn Filesystem> = match root {
        Some(volume) => Arc::new(overlay::Overlay::new(volume)),
        None => Arc::new(ramfs::RamFs::new()),
    };
    let mut abi = Vfs::new();
    let _ = abi.mount(fhs::mount::ROOT, abi_root, plain);
    let _ = abi.mount(fhs::mount::TMP, tmp, plain);
    if let Some(volume) = data {
        let _ = abi.mount(DATA_MOUNT, volume, plain);
    }
    for (point, name) in abi.mounts() {
        serial_println!("fs: abi mounted {name} at {point}");
    }
    Tables {
        native,
        abi,
        mounted,
    }
}

fn log_mounts(native: &Vfs, abi: &Vfs) {
    for (point, name) in native.mounts() {
        serial_println!(
            "fs: mounted {name} at {point}{}",
            native.mount_flags(&point).proc_suffix()
        );
    }
    for (point, name) in abi.mounts() {
        serial_println!("fs: abi mounted {name} at {point}");
    }
}

fn describe(id: &VolumeId) -> alloc::string::String {
    match id {
        VolumeId::Uuid(uuid) => fmt_uuid(uuid),
        VolumeId::Label(label) => {
            let end = label.iter().position(|&b| b == 0).unwrap_or(label.len());
            alloc::string::String::from_utf8_lossy(&label[..end]).into_owned()
        }
    }
}

fn fmt_uuid(uuid: &[u8; 16]) -> alloc::string::String {
    use core::fmt::Write;
    let mut text = alloc::string::String::new();
    for (index, byte) in uuid.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        let _ = write!(text, "{byte:02x}");
    }
    text
}
