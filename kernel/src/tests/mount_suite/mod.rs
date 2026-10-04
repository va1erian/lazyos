//! Boot config, mount flags and the mount-table layouts (docs/filesystem-plan.md
//! F1). Volumes are `FakeDisk` images built here: a FAT boot volume carrying
//! `lazyos.cfg`, and ext2 volumes with chosen UUIDs and labels.

use super::block_suite::FakeDisk;
use super::ext2_suite::{mkfs, DISK_SECTORS, SUPER};
use super::fs_suite::fat_image::image_with_file;
use super::*;
use crate::block::SECTOR_SIZE;

mod config;
mod flags;
mod layout;
mod library_image;
mod power_cycle;
mod ramdisk_root;

pub(super) const CASES: &[(&str, Test)] = &[
    ("mount_cfg_parses_every_key", config::parses_every_key),
    ("mount_cfg_hostile_inputs", config::hostile_inputs),
    ("mount_flag_lists", flags::flag_lists),
    (
        "mount_ro_refuses_every_mutation",
        flags::ro_refuses_every_mutation,
    ),
    ("mount_flags_reported", flags::flags_are_reported),
    (
        "mount_readdir_lists_mount_points",
        flags::readdir_lists_mount_points,
    ),
    (
        "mount_proc_mounts_shows_flags",
        flags::proc_mounts_shows_flags,
    ),
    (
        "mount_legacy_layout_without_config",
        layout::legacy_without_config,
    ),
    (
        "mount_legacy_layout_unknown_root",
        layout::legacy_with_unknown_root,
    ),
    ("mount_configured_layout", layout::configured_layout),
    ("mount_configured_layout_missing_home", layout::missing_home),
    ("mount_readonly_device_root", layout::readonly_device_root),
    ("mount_cycles_soak", layout::mount_cycles_soak),
    (
        "mount_library_formatted_root",
        library_image::library_formatted_root_mounts,
    ),
    (
        "mount_root_power_cycle_marks_clean",
        power_cycle::root_power_cycle_marks_clean,
    ),
    (
        "mount_root_unclean_stays_flagged_until_checked",
        power_cycle::unclean_root_stays_flagged_until_checked,
    ),
    ("mount_root_power_cycle_soak", power_cycle::power_cycle_soak),
    ("mount_ramdisk_root_wins", ramdisk_root::ramdisk_root_wins),
    (
        "mount_ramdisk_partition_names",
        ramdisk_root::partition_names,
    ),
    (
        "mount_ramdisk_bare_does_not_win",
        ramdisk_root::bare_ramdisk_does_not_win,
    ),
    (
        "mount_ramdisk_preference_soak",
        ramdisk_root::soak_preference_is_deterministic,
    ),
];

/// A UUID whose bytes are `seed` repeated, with the matching text form.
pub(super) fn uuid(seed: u8) -> ([u8; 16], String) {
    let bytes = [seed; 16];
    let hex = |n: usize| format!("{seed:02x}").repeat(n);
    (
        bytes,
        format!("{}-{}-{}-{}-{}", hex(4), hex(2), hex(2), hex(2), hex(6)),
    )
}

/// A freshly formatted (clean) ext2 image with the given UUID and label.
pub(super) fn ext2_image(uuid: [u8; 16], label: &str) -> Vec<u8> {
    let mut image = mkfs(1024, 512, 64);
    image[SUPER + 0x68..SUPER + 0x78].copy_from_slice(&uuid);
    image[SUPER + 0x78..SUPER + 0x88].fill(0); // mkfs names the volume itself
    image[SUPER + 0x78..SUPER + 0x78 + label.len()].copy_from_slice(label.as_bytes());
    image
}

/// An ext2 volume on a fresh `FakeDisk` with the given UUID and label.
fn ext2_disk(name: &'static str, uuid: [u8; 16], label: &str) -> &'static FakeDisk {
    let disk = FakeDisk::new(name, DISK_SECTORS);
    disk.data.lock().copy_from_slice(&ext2_image(uuid, label));
    disk
}

/// The disks every layout test mounts. Built once: the test heap is small and
/// a `FakeDisk` is leaked, so each test reuses these (a test may dirty them).
pub(super) struct Rig {
    pub(super) boot: &'static FakeDisk,
    pub(super) root: &'static FakeDisk,
    pub(super) home: &'static FakeDisk,
}

/// The shared [`Rig`], with `lazyos.cfg` on its boot volume set to `config`
/// (or absent: some other file instead).
pub(super) fn rig(config: Option<&str>) -> &'static Rig {
    static RIG: spin::Once<Rig> = spin::Once::new();
    let image = match config {
        Some(text) => image_with_file(b"LAZYOS  CFG", text.as_bytes()),
        None => image_with_file(b"OTHER   TXT", b"x"),
    };
    let rig = RIG.call_once(|| Rig {
        boot: FakeDisk::new("mt-boot", image.len() / SECTOR_SIZE),
        root: ext2_disk("mt-root", uuid(0xA1).0, "rootfs"),
        home: ext2_disk("mt-home", uuid(0xB2).0, "home"),
    });
    rig.boot.data.lock().copy_from_slice(&image);
    rig.root.set_read_only(false);
    rig
}
