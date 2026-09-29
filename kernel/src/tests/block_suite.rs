//! Block devices (issue #100).

use super::*;
use crate::block::{self, BlockDevice, BlockError, SECTOR_SIZE};
use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::Mutex;

/// An in-memory [`BlockDevice`]: pins the trait's read/write/flush/bounds
/// contract without hardware. Leaked so the registry can hold it forever.
/// The ext2 suite reuses it as the backing store for formatted images.
pub(super) struct FakeDisk {
    name: &'static str,
    pub(super) data: Mutex<Vec<u8>>,
    writes: AtomicU32,
    pub(super) flushes: AtomicU32,
    /// Remaining writes before an injected failure, or `u32::MAX` (the
    /// default) for "never fail". `1` fails the very next write, then
    /// resets to "never" so the disk behaves normally again -- one write
    /// failure is exactly what a real transient I/O error looks like.
    fail_in: AtomicU32,
    /// Report the device as unwritable, as a read-only attach would.
    read_only: AtomicBool,
}

impl FakeDisk {
    pub(super) fn new(name: &'static str, sectors: usize) -> &'static FakeDisk {
        Box::leak(Box::new(FakeDisk {
            name,
            data: Mutex::new(vec![0u8; sectors * SECTOR_SIZE]),
            writes: AtomicU32::new(0),
            flushes: AtomicU32::new(0),
            fail_in: AtomicU32::new(u32::MAX),
            read_only: AtomicBool::new(false),
        }))
    }

    /// Make the device claim it cannot be written (or writable again), so a
    /// filesystem opened next mounts read-only.
    pub(super) fn set_read_only(&self, read_only: bool) {
        self.read_only.store(read_only, Ordering::Relaxed);
    }

    /// Fail the `n`th write from now (`n == 1` is the very next one), then
    /// resume succeeding. Test-only fault injection for the ext2 short-write
    /// suite; no production code path can trigger a device write failure on
    /// demand.
    pub(super) fn fail_nth_write(&self, n: u32) {
        self.fail_in.store(n, Ordering::Relaxed);
    }
}

impl BlockDevice for FakeDisk {
    fn name(&self) -> &'static str {
        self.name
    }

    fn sector_count(&self) -> u64 {
        (self.data.lock().len() / SECTOR_SIZE) as u64
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        self.check_range(lba, buf.len())?;
        let data = self.data.lock();
        let start = lba as usize * SECTOR_SIZE;
        buf.copy_from_slice(&data[start..start + buf.len()]);
        Ok(())
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        self.check_range(lba, buf.len())?;
        // Fault injection: consume one "countdown" tick per write attempt,
        // regardless of outcome, so `fail_nth_write(n)` always means the
        // n-th write call from when it was armed, not the n-th successful
        // one.
        let remaining = self.fail_in.load(Ordering::Relaxed);
        if remaining != u32::MAX {
            if remaining <= 1 {
                self.fail_in.store(u32::MAX, Ordering::Relaxed);
                return Err(BlockError::Io);
            }
            self.fail_in.store(remaining - 1, Ordering::Relaxed);
        }
        let mut data = self.data.lock();
        let start = lba as usize * SECTOR_SIZE;
        data[start..start + buf.len()].copy_from_slice(buf);
        self.writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn flush(&self) -> Result<(), BlockError> {
        self.flushes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn is_writable(&self) -> bool {
        !self.read_only.load(Ordering::Relaxed)
    }
}

/// The trait's data path on a fake device: write, read back, flush, and
/// the bounds/misalignment errors.
pub fn fake_read_write_flush() -> Result<(), String> {
    let disk = FakeDisk::new("test-fake0", 8);
    check!(
        block::register(disk).is_ok(),
        "registering the fake disk failed"
    );
    check!(
        disk.sector_size() == SECTOR_SIZE && disk.sector_count() == 8,
        "fake geometry is {} sectors of {}, expected 8 of {SECTOR_SIZE}",
        disk.sector_count(),
        disk.sector_size()
    );
    check!(disk.is_writable(), "the fake disk claims to be read-only");

    let mut sector = [0u8; SECTOR_SIZE];
    for (index, byte) in sector.iter_mut().enumerate() {
        *byte = (index as u8) ^ 0x5A;
    }
    disk.write_sectors(3, &sector)
        .map_err(|error| format!("write failed: {error:?}"))?;
    check!(
        disk.writes.load(Ordering::Relaxed) == 1,
        "the write counter did not move"
    );
    let mut readback = [0u8; SECTOR_SIZE];
    disk.read_sectors(3, &mut readback)
        .map_err(|error| format!("read failed: {error:?}"))?;
    check!(
        readback == sector,
        "read back different bytes than were written"
    );
    disk.flush()
        .map_err(|error| format!("flush failed: {error:?}"))?;
    check!(
        disk.flushes.load(Ordering::Relaxed) == 1,
        "flush did not reach the device"
    );

    check!(
        disk.read_sectors(8, &mut readback).err() == Some(BlockError::Bounds),
        "reading past the last sector was not Bounds"
    );
    check!(
        disk.read_sectors(7, &mut [0u8; 2 * SECTOR_SIZE]).err() == Some(BlockError::Bounds),
        "a range straddling the end was not Bounds"
    );
    check!(
        disk.write_sectors(0, &[0u8; 100]).err() == Some(BlockError::Unsupported),
        "a partial sector was not Unsupported"
    );
    check!(
        disk.read_sectors(0, &mut []).is_ok(),
        "an empty transfer failed"
    );
    Ok(())
}

/// Registration, lookup by name, duplicate rejection, listing, and the
/// boot-device selection.
pub fn registry_register_lookup_duplicate() -> Result<(), String> {
    let first = FakeDisk::new("test-registry-a", 4);
    let second = FakeDisk::new("test-registry-b", 4);
    check!(block::register(first).is_ok(), "registering a failed");
    check!(block::register(second).is_ok(), "registering b failed");
    check!(
        block::register(first).err() == Some(BlockError::Exists),
        "a duplicate device name was accepted"
    );
    check!(
        block::device("test-registry-a").map(|dev| dev.name()) == Some("test-registry-a"),
        "lookup by name failed"
    );
    check!(
        block::device("test-registry-missing").is_none(),
        "an unknown device name matched"
    );
    let names: Vec<&str> = block::devices().iter().map(|dev| dev.name()).collect();
    check!(
        names.contains(&"test-registry-a") && names.contains(&"test-registry-b"),
        "devices() is {names:?}"
    );
    // The boot device is a process-wide global, so put it back whatever the
    // check says: later suites open the real boot disk through it.
    let previous = block::boot_device();
    block::set_boot_device(first);
    let stuck = block::boot_device().map(|dev| dev.name()) == Some("test-registry-a");
    if let Some(device) = previous {
        block::set_boot_device(device);
    }
    check!(stuck, "set_boot_device did not stick");
    Ok(())
}

/// The ATA path still reads the boot disk through the trait: sector 0
/// carries a valid MBR with a FAT partition, and that partition's boot
/// sector is a 512-byte-per-sector BPB. This is the acceptance for "the
/// default image boots from ATA through the block layer".
pub fn ata_reads_fat_root() -> Result<(), String> {
    block::init();
    let device = block::device("ata0").ok_or_else(|| String::from("ata0 is not registered"))?;
    check!(device.sector_count() > 0, "ata0 reports an empty geometry");

    let mut mbr = [0u8; SECTOR_SIZE];
    device
        .read_sectors(0, &mut mbr)
        .map_err(|error| format!("MBR read failed: {error:?}"))?;
    check!(
        mbr[510] == 0x55 && mbr[511] == 0xAA,
        "sector 0 has no MBR signature"
    );

    let mut fat_lba = None;
    for index in 0..4 {
        let base = 0x1BE + index * 16;
        let kind = mbr[base + 4];
        let start =
            u32::from_le_bytes([mbr[base + 8], mbr[base + 9], mbr[base + 10], mbr[base + 11]]);
        let sectors = u32::from_le_bytes([
            mbr[base + 12],
            mbr[base + 13],
            mbr[base + 14],
            mbr[base + 15],
        ]);
        let is_fat = matches!(kind, 0x01 | 0x04 | 0x06 | 0x0B | 0x0C);
        if is_fat && sectors > 0 {
            fat_lba = Some(start);
            break;
        }
    }
    let lba = fat_lba.ok_or_else(|| String::from("the MBR carries no FAT partition"))?;

    let mut bpb = [0u8; SECTOR_SIZE];
    device
        .read_sectors(u64::from(lba), &mut bpb)
        .map_err(|error| format!("BPB read failed: {error:?}"))?;
    let bytes_per_sector = u16::from_le_bytes([bpb[11], bpb[12]]);
    check!(
        bytes_per_sector == SECTOR_SIZE as u16,
        "the BPB says {bytes_per_sector} bytes per sector"
    );

    // The `mount <dev>` surface: the boot device can be mounted at a new
    // point; duplicate points and non-boot devices are refused. This also
    // proves the filesystem layer reached the disk through the registry.
    check!(
        crate::fs::init(),
        "the FAT volume did not mount through the block layer"
    );
    check!(
        crate::fs::mount_device("/mnt", "ata0").is_ok(),
        "mounting ata0 at /mnt failed"
    );
    check!(
        crate::fs::mount_device("/mnt", "ata0").err() == Some(crate::fs::vfs::FsError::Exists),
        "a duplicate mount point was accepted"
    );
    check!(
        crate::fs::mount_device("/mnt2", "test-registry-a").err()
            == Some(crate::fs::vfs::FsError::NotSupported),
        "a non-boot device was mounted"
    );
    check!(
        crate::fs::mount_device("/mnt3", "no-such-device").err()
            == Some(crate::fs::vfs::FsError::NotFound),
        "an unknown device was mounted"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("block_fake_read_write_flush", fake_read_write_flush),
    (
        "block_registry_register_lookup_duplicate",
        registry_register_lookup_duplicate,
    ),
    ("block_ata_reads_fat_root", ata_reads_fat_root),
];
