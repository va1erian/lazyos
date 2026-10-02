//! The ramdisk goes first (docs/usb-stick.md): when the bootloader handed over
//! a ramdisk, its boot volume and root win over a disk carrying a volume with
//! the same UUID, whatever order the devices registered in.

use super::*;
use crate::block::BlockDevice;
use crate::fs::mounts::{self, is_disk_or_partition};

/// One of the shared rig disks under another name: the ramdisk's volumes
/// carry the same bytes as the disk's (the same root UUID is the point), and
/// the suite's heap stays free of more leaked images (`fsops` later needs
/// megabyte-sized blocks of it).
struct Alias {
    name: &'static str,
    disk: &'static FakeDisk,
}

impl BlockDevice for Alias {
    fn name(&self) -> &'static str {
        self.name
    }
    fn sector_count(&self) -> u64 {
        self.disk.sector_count()
    }
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), crate::block::BlockError> {
        self.disk.read_sectors(lba, buf)
    }
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), crate::block::BlockError> {
        self.disk.write_sectors(lba, buf)
    }
    fn flush(&self) -> Result<(), crate::block::BlockError> {
        self.disk.flush()
    }
}

/// A ramdisk's two volumes, `mt-rd` and `mt-rdp2`: the rig's boot volume
/// (whose `lazyos.cfg` names root UUID 0xA1) and the rig's root (UUID 0xA1).
struct RamRig {
    boot: Alias,
    root: Alias,
}

fn ram_rig(rig: &'static Rig) -> &'static RamRig {
    static RIG: spin::Once<RamRig> = spin::Once::new();
    RIG.call_once(|| RamRig {
        boot: Alias {
            name: "mt-rd",
            disk: rig.boot,
        },
        root: Alias {
            name: "mt-rdp2",
            disk: rig.root,
        },
    })
}

/// Every order the four devices can be met in that matters: disks first,
/// ramdisk first, and interleaved.
fn orders(rig: &'static Rig, ram: &'static RamRig) -> [[&'static dyn BlockDevice; 4]; 3] {
    [
        [rig.boot, rig.root, &ram.boot, &ram.root],
        [&ram.boot, &ram.root, rig.boot, rig.root],
        [rig.boot, &ram.root, rig.root, &ram.boot],
    ]
}

/// The ramdisk's root wins in every order; without the preference the first
/// boot volume met decides, which is why the rule exists.
pub fn ramdisk_root_wins() -> Result<(), String> {
    let rig = rig(Some(&format!("root=UUID={}\n", uuid(0xA1).1)));
    let ram = ram_rig(rig);
    for (index, devices) in orders(rig, ram).iter().enumerate() {
        let tables = mounts::build_preferring(devices, Some("mt-rd"));
        check!(
            tables.root_device == Some("mt-rdp2"),
            "order {index}: root on {:?}",
            tables.root_device
        );
        check!(
            tables.native.mounts().len() == 4,
            "order {index}: mounts {:?}",
            tables.native.mounts()
        );
    }
    let disks_first = &orders(rig, ram)[0];
    let tables = mounts::build_preferring(disks_first, None);
    check!(
        tables.root_device == Some("mt-root"),
        "no preference: root on {:?}",
        tables.root_device
    );
    Ok(())
}

/// A ramdisk with no `lazyos.cfg` (no FAT boot volume at all here; a bare
/// `LAZYOS_RAMDISK` FAT image is the same case) changes nothing: the disk's
/// configured layout still mounts its own root.
pub fn bare_ramdisk_does_not_win() -> Result<(), String> {
    static BARE: spin::Once<Alias> = spin::Once::new();
    let rig = rig(Some(&format!("root=UUID={}\n", uuid(0xA1).1)));
    let bare = BARE.call_once(|| Alias {
        name: "mt-bare",
        disk: rig.home,
    });
    // Registered after the disk, as the test kernel's ramdisk is: preferring
    // it regardless would put it first.
    let devices: [&'static dyn BlockDevice; 3] = [rig.boot, rig.root, bare];
    let tables = mounts::build_preferring(&devices, Some("mt-bare"));
    check!(
        tables.root_device == Some("mt-root"),
        "root on {:?}",
        tables.root_device
    );
    Ok(())
}

/// Which names belong to the preferred disk: itself and `<disk>p<digits>`.
pub fn partition_names() -> Result<(), String> {
    let cases = [
        ("ram0", true),
        ("ram0p2", true),
        ("ram0p15", true),
        ("ram0p", false),
        ("ram01", false),
        ("ram0px", false),
        ("ram", false),
        ("virtio0p2", false),
    ];
    for (name, want) in cases {
        check!(
            is_disk_or_partition(name, "ram0") == want,
            "{name}: expected {want}"
        );
    }
    Ok(())
}

/// Soak: 300 builds in rotating orders all pick the same root.
pub fn soak_preference_is_deterministic() -> Result<(), String> {
    let rig = rig(Some(&format!("root=UUID={}\n", uuid(0xA1).1)));
    let ram = ram_rig(rig);
    let orders = orders(rig, ram);
    let _ = mounts::build_preferring(&orders[0], Some("mt-rd")); // warm caches
    let measure = || crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used;
    let before = measure();
    for round in 0..300usize {
        let mut devices = orders[round % orders.len()];
        let shift = round % devices.len();
        devices.rotate_left(shift);
        let tables = mounts::build_preferring(&devices, Some("mt-rd"));
        check!(
            tables.root_device == Some("mt-rdp2"),
            "round {round}: root on {:?}",
            tables.root_device
        );
    }
    let after = measure();
    check!(
        after <= before + 16 * 1024,
        "300 builds grew the heap from {before} to {after}"
    );
    Ok(())
}
