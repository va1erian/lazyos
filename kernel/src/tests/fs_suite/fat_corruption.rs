//! FAT12/16 robustness: malformed BPB/geometry, corrupt chain pointers, and
//! per-volume device handles (issues #235, #244).
//!
//! The on-disk bytes here are attacker-controlled — a USB stick or disk image
//! can carry anything — so every case below must answer with an error or a
//! bounded read instead of panicking, silently reading the wrong disk, or
//! looping with interrupts off.

use super::*;
use crate::block::{BlockDevice, SECTOR_SIZE};
use crate::fs::fat::Fat16;
use crate::fs::vfs::Filesystem;
use crate::tests::block_suite::FakeDisk;
use alloc::vec;
use alloc::vec::Vec;

/// Geometry of the synthetic FAT12 image used below.
const DISK_SECTORS: usize = 40;
const PART_LBA: usize = 1;
const PART_SECTORS: u32 = 32;
const FAT_LBA: usize = 2;
const ROOT_LBA: usize = 3;
const DATA_LBA: usize = 4;
const CLUSTER_BYTES: u32 = 512;
const PAYLOAD: &[u8] = b"hello fat corruption";

pub(super) fn put16(buf: &mut [u8], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn put32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// Write a packed 12-bit FAT entry (entries share a byte at odd boundaries).
pub(super) fn set_fat12(fat: &mut [u8], cluster: u16, value: u16) {
    let offset = cluster as usize + cluster as usize / 2;
    let word = u16::from_le_bytes([fat[offset], fat[offset + 1]]);
    let word = if cluster.is_multiple_of(2) {
        (word & 0xF000) | (value & 0x0FFF)
    } else {
        (word & 0x000F) | ((value & 0x0FFF) << 4)
    };
    fat[offset..offset + 2].copy_from_slice(&word.to_le_bytes());
}

/// A well-formed MBR + FAT12 volume with one two-cluster file `DATA.BIN`.
fn fat12_image() -> Vec<u8> {
    let mut img = vec![0u8; DISK_SECTORS * SECTOR_SIZE];

    // MBR partition table entry 0: FAT12 at PART_LBA.
    let entry = 0x1BE;
    img[entry + 4] = 0x01;
    put32(&mut img, entry + 8, PART_LBA as u32);
    put32(&mut img, entry + 12, PART_SECTORS);
    img[510] = 0x55;
    img[511] = 0xAA;

    // BPB.
    let bpb = PART_LBA * SECTOR_SIZE;
    put16(&mut img, bpb + 11, SECTOR_SIZE as u16);
    img[bpb + 13] = 1; // sectors per cluster
    put16(&mut img, bpb + 14, 1); // reserved sectors
    img[bpb + 16] = 1; // FAT copies
    put16(&mut img, bpb + 17, 16); // root entries -> one root sector
    put16(&mut img, bpb + 19, PART_SECTORS as u16);
    put16(&mut img, bpb + 22, 1); // sectors per FAT

    // FAT: cluster 2 chains to 3; cluster 3 ends the chain.
    let fat = FAT_LBA * SECTOR_SIZE;
    set_fat12(&mut img[fat..fat + SECTOR_SIZE], 0, 0xFF8);
    set_fat12(&mut img[fat..fat + SECTOR_SIZE], 1, 0xFFF);
    set_fat12(&mut img[fat..fat + SECTOR_SIZE], 2, 3);
    set_fat12(&mut img[fat..fat + SECTOR_SIZE], 3, 0xFFF);

    // Root directory entry.
    let root = ROOT_LBA * SECTOR_SIZE;
    img[root..root + 8].copy_from_slice(b"DATA    ");
    img[root + 8..root + 11].copy_from_slice(b"BIN");
    img[root + 11] = 0x20; // archive, not a directory
    put16(&mut img, root + 26, 2);
    put32(&mut img, root + 28, PAYLOAD.len() as u32);

    // Data clusters 2 and 3.
    img[DATA_LBA * SECTOR_SIZE..DATA_LBA * SECTOR_SIZE + PAYLOAD.len()].copy_from_slice(PAYLOAD);
    img[(DATA_LBA + 1) * SECTOR_SIZE..(DATA_LBA + 1) * SECTOR_SIZE + 8]
        .copy_from_slice(b"CLUSTER3");

    img
}

/// Install `image` into `disk` and open the volume, panicking on a bad image.
fn open_image(disk: &'static FakeDisk, image: &[u8]) -> Fat16 {
    disk.data.lock().copy_from_slice(image);
    Fat16::open(disk).expect("the synthetic FAT12 image should open")
}

/// Overwrite the sole root entry's cluster pointer and size.
fn set_root_entry(img: &mut [u8], cluster: u16, size: u32) {
    let root = ROOT_LBA * SECTOR_SIZE;
    put16(img, root + 26, cluster);
    put32(img, root + 28, size);
}

/// A malformed BPB (overflowing or impossible geometry) is refused rather
/// than trusted; the same image with sane fields opens.
pub fn fat_malformed_geometry_rejected() -> Result<(), String> {
    let disk = FakeDisk::new("test-fat-geometry", DISK_SECTORS);
    let good = fat12_image();
    open_image(disk, &good); // must not panic

    let bpb = PART_LBA * SECTOR_SIZE;

    // 16-bit FAT size is zero, so the parser falls back to the 32-bit field;
    // a 0xFFFF_FFFF value must not overflow the LBA math (issue #235.1).
    let mut img = good.clone();
    put16(&mut img, bpb + 22, 0);
    put32(&mut img, bpb + 36, 0xFFFF_FFFF);
    disk.data.lock().copy_from_slice(&img);
    check!(
        Fat16::open(disk).is_none(),
        "an overflowing FAT size was accepted"
    );

    // A huge root-directory count leaves less than one data cluster.
    let mut img = good.clone();
    put16(&mut img, bpb + 17, 0xFFFF);
    disk.data.lock().copy_from_slice(&img);
    check!(
        Fat16::open(disk).is_none(),
        "an impossible root-directory size was accepted"
    );

    // Zero total sectors means no data at all.
    let mut img = good.clone();
    put16(&mut img, bpb + 19, 0);
    put32(&mut img, bpb + 32, 0);
    disk.data.lock().copy_from_slice(&img);
    check!(
        Fat16::open(disk).is_none(),
        "a zero-length volume was accepted"
    );
    Ok(())
}

/// The MBR partition extents are checked against the device, so a partition
/// that runs past the end of the disk is skipped (issue #235.4).
pub fn fat_partition_bounds_checked() -> Result<(), String> {
    let disk = FakeDisk::new("test-fat-bounds", DISK_SECTORS);

    let mut img = fat12_image();
    put32(&mut img, 0x1BE + 12, 0xFFFF_FFFF); // sectors
    disk.data.lock().copy_from_slice(&img);
    check!(
        Fat16::open(disk).is_none(),
        "a partition past the device end was accepted"
    );

    let mut img = fat12_image();
    put32(&mut img, 0x1BE + 8, 0xFFFF_FF00); // lba
    disk.data.lock().copy_from_slice(&img);
    check!(
        Fat16::open(disk).is_none(),
        "a partition starting past the device end was accepted"
    );
    Ok(())
}

/// Every implausible chain pointer — reserved cluster 1, the bad-cluster
/// marker, and a value past the last cluster — ends the read with an error
/// instead of underflowing `cluster - 2` or reading outside the data area.
pub fn fat_chain_pointers_validated() -> Result<(), String> {
    let disk = FakeDisk::new("test-fat-chain", DISK_SECTORS);

    for (name, pointer) in [("reserved", 1u16), ("bad", 0xFF7), ("out-of-range", 40)] {
        let mut img = fat12_image();
        let fat = FAT_LBA * SECTOR_SIZE;
        set_fat12(&mut img[fat..fat + SECTOR_SIZE], 2, pointer);
        // A size spanning two clusters forces the walk to follow FAT[2].
        set_root_entry(&mut img, 2, 2 * CLUSTER_BYTES);
        let volume = open_image(disk, &img);
        let mut buf = [0u8; 1024];
        check!(
            volume.read("DATA.BIN", 0, &mut buf).is_err(),
            "a {name} chain pointer ({pointer:#x}) was followed"
        );
    }
    Ok(())
}

/// A chain that ends before the directory entry's claimed size is an error,
/// and a size larger than the whole volume is refused before it can drive a
/// giant allocation or a long walk (issue #235.3).
pub fn fat_chain_length_and_size_bounded() -> Result<(), String> {
    let disk = FakeDisk::new("test-fat-length", DISK_SECTORS);

    // Cluster 2 ends the chain, but the entry claims two clusters.
    let mut img = fat12_image();
    let fat = FAT_LBA * SECTOR_SIZE;
    set_fat12(&mut img[fat..fat + SECTOR_SIZE], 2, 0xFFF);
    set_root_entry(&mut img, 2, 2 * CLUSTER_BYTES);
    let volume = open_image(disk, &img);
    let mut buf = [0u8; 1024];
    check!(
        volume.read("DATA.BIN", 0, &mut buf).is_err(),
        "a truncated chain was accepted"
    );

    // A 4 GiB claim exceeds the 29-cluster volume: rejected up front.
    let mut img = fat12_image();
    set_root_entry(&mut img, 2, u32::MAX);
    let volume = open_image(disk, &img);
    check!(
        volume.read("DATA.BIN", 0, &mut buf).is_err(),
        "a size beyond the volume capacity was accepted"
    );

    // A cyclic chain with an absurd size must also be refused, not walked.
    let mut img = fat12_image();
    set_fat12(&mut img[fat..fat + SECTOR_SIZE], 2, 2);
    set_root_entry(&mut img, 2, u32::MAX);
    let volume = open_image(disk, &img);
    check!(
        volume.read("DATA.BIN", 0, &mut buf).is_err(),
        "a cyclic chain with an oversized file was accepted"
    );
    Ok(())
}

/// A mounted FAT volume reads through the device it was opened from, not the
/// block layer's current boot device (issue #244): two disks carry the same
/// layout with different payloads, and swapping the boot device must not
/// change what the first volume returns.
pub fn fat_volume_uses_its_own_device() -> Result<(), String> {
    let first = FakeDisk::new("test-fat-device-a", DISK_SECTORS);
    let second = FakeDisk::new("test-fat-device-b", DISK_SECTORS);

    let mut image_a = fat12_image();
    image_a[DATA_LBA * SECTOR_SIZE..DATA_LBA * SECTOR_SIZE + PAYLOAD.len()]
        .copy_from_slice(PAYLOAD);
    let mut image_b = fat12_image();
    let payload_b = b"second disk content!";
    image_b[DATA_LBA * SECTOR_SIZE..DATA_LBA * SECTOR_SIZE + payload_b.len()]
        .copy_from_slice(payload_b);
    first.data.lock().copy_from_slice(&image_a);
    second.data.lock().copy_from_slice(&image_b);

    let previous = crate::block::boot_device();
    crate::block::set_boot_device(second);
    let result = boot_device_swap_body(first, second, payload_b);
    // Restore the global even on failure, so later tests and `init` see the
    // device they expect.
    if let Some(device) = previous {
        crate::block::set_boot_device(device);
    }
    result?;
    mount_device_accepts_non_boot_fat(payload_b)
}

/// The body of [`fat_volume_uses_its_own_device`] that runs while the boot
/// device points at the *other* disk, as a probe on it would.
fn boot_device_swap_body(
    first: &'static FakeDisk,
    second: &'static FakeDisk,
    payload_b: &[u8],
) -> Result<(), String> {
    let volume_a = Fat16::open(first).ok_or_else(|| String::from("volume A did not open"))?;
    let mut buf = [0u8; 64];
    let read = volume_a
        .read("DATA.BIN", 0, &mut buf)
        .map_err(|error| format!("read from volume A failed: {error:?}"))?;
    check!(
        buf[..read].starts_with(PAYLOAD),
        "volume A read {} bytes from the wrong device: {:?}",
        read,
        &buf[..read]
    );

    // And a volume opened from the second disk sees the second payload.
    let volume_b = Fat16::open(second).ok_or_else(|| String::from("volume B did not open"))?;
    let read = volume_b
        .read("DATA.BIN", 0, &mut buf)
        .map_err(|error| format!("read from volume B failed: {error:?}"))?;
    check!(
        buf[..read].starts_with(payload_b),
        "volume B read {} bytes from the wrong device: {:?}",
        read,
        &buf[..read]
    );
    Ok(())
}

/// `mount_device` must mount FAT from any registered device, not only the
/// boot device (issue #244): the second disk is not the boot device here, yet
/// its FAT volume mounts and serves its own payload.
fn mount_device_accepts_non_boot_fat(payload: &[u8]) -> Result<(), String> {
    crate::task::register_kernel();
    crate::fs::init();
    let disk = FakeDisk::new("test-fat-mount-b", DISK_SECTORS);
    let mut image = fat12_image();
    image[DATA_LBA * SECTOR_SIZE..DATA_LBA * SECTOR_SIZE + payload.len()].copy_from_slice(payload);
    disk.data.lock().copy_from_slice(&image);
    check!(
        crate::block::register(disk).is_ok(),
        "registering the FAT disk failed"
    );
    let is_boot = crate::block::boot_device().is_some_and(|boot| boot.name() == disk.name());
    check!(!is_boot, "the test disk must not be the boot device");
    check!(
        crate::fs::mount_device("/fatb", "test-fat-mount-b").is_ok(),
        "mount_device refused a non-boot FAT volume"
    );
    let data = crate::fs::vfs_read(Id::ROOT, "/fatb/DATA.BIN").map_err(fs_error)?;
    check!(
        data.starts_with(payload),
        "the mounted volume returned {data:?}"
    );
    Ok(())
}
