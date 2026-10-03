//! MBR partitions as block devices (docs/filesystem-plan.md F1).
//!
//! After the drivers attach, every whole-disk device has its MBR parsed and
//! each usable entry is registered as `<disk>p<n>` (1-based like the MBR
//! index). The partition table is untrusted input: an entry is used only when
//! its extent lies wholly inside the disk and overlaps no other entry, and a
//! bad one is logged and skipped, never clamped.
//!
//! Slots are a fixed static pool so the registry keeps holding
//! `&'static dyn BlockDevice` without a heap. Only whole disks are scanned,
//! never partitions; GPT/extended tables are out of scope. [`scan_all`] skips
//! `ram0`, which [`super::mem::register_ramdisk`] scans itself because it
//! registers after the drivers; [`scan_disk`] is the entry point for any
//! device that appears later (the USB stick through `usbd`, track 5).

use super::{check_range, BlockDevice, BlockError, SECTOR_SIZE};
use core::str;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Once;

/// How many partition devices the pool holds.
pub const MAX_PARTITIONS: usize = 16;

const TABLE_OFFSET: usize = 446;
const ENTRIES: usize = 4;
const ENTRY_LEN: usize = 16;
const SIGNATURE_OFFSET: usize = 510;

const TYPE_EXTENDED: [u8; 2] = [0x05, 0x0F];
const TYPE_GPT_PROTECTIVE: u8 = 0xEE;
const TYPE_LINUX: u8 = 0x83;
const TYPES_FAT: [u8; 5] = [0x01, 0x04, 0x06, 0x0B, 0x0C];

/// One validated MBR entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    /// 1-based MBR slot.
    pub index: u8,
    pub kind: u8,
    pub lba: u64,
    pub sectors: u64,
}

impl Entry {
    fn end(&self) -> u64 {
        self.lba + self.sectors
    }

    fn overlaps(&self, other: &Entry) -> bool {
        self.lba < other.end() && other.lba < self.end()
    }
}

/// Parse an MBR sector into the entries worth registering for a disk of
/// `disk_sectors`. Pure: no I/O, and logging is the only side effect.
pub fn parse_mbr(
    mbr: &[u8; SECTOR_SIZE],
    disk_sectors: u64,
    disk: &str,
) -> [Option<Entry>; ENTRIES] {
    let mut found = [None; ENTRIES];
    if mbr[SIGNATURE_OFFSET..] != [0x55, 0xAA] || is_fat_boot_record(mbr) {
        return found;
    }
    let table = &mbr[TABLE_OFFSET..SIGNATURE_OFFSET];
    for (slot, raw) in table.as_chunks::<ENTRY_LEN>().0.iter().enumerate() {
        let index = slot as u8 + 1;
        let kind = raw[4];
        let lba = u64::from(u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]));
        let sectors = u64::from(u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]));
        if kind == 0 {
            continue;
        }
        if TYPE_EXTENDED.contains(&kind) {
            serial_println!("block: {disk}p{index}: extended partitions are not supported");
            continue;
        }
        if kind == TYPE_GPT_PROTECTIVE {
            serial_println!("block: {disk}: GPT is not supported (protective MBR)");
            continue;
        }
        if kind != TYPE_LINUX && !TYPES_FAT.contains(&kind) {
            continue;
        }
        // `lba` and `sectors` are u32 widened to u64, so the sum cannot wrap,
        // but the checked form keeps the rule visible.
        let fits = sectors > 0
            && lba >= 1
            && lba
                .checked_add(sectors)
                .is_some_and(|end| end <= disk_sectors);
        if !fits {
            serial_println!(
                "block: {disk}p{index}: extent {lba}+{sectors} outside the disk, skipped"
            );
            continue;
        }
        found[slot] = Some(Entry {
            index,
            kind,
            lba,
            sectors,
        });
    }
    // Neither of an overlapping pair can be trusted.
    let snapshot = found;
    for entry in found.iter_mut() {
        let clash = snapshot.iter().flatten().any(|other| {
            Some(other) != entry.as_ref() && entry.as_ref().is_some_and(|e| e.overlaps(other))
        });
        if clash {
            if let Some(bad) = entry.take() {
                serial_println!(
                    "block: {disk}p{}: overlaps another partition, skipped",
                    bad.index
                );
            }
        }
    }
    found
}

/// A FAT volume boot record (a bare FAT image, like the issue #5 ramdisk) also
/// ends in 0x55AA, but its bytes 446.. are boot code, not a partition table:
/// a jump instruction plus the `FAT` type string of a FAT12/16 or FAT32 BPB.
fn is_fat_boot_record(sector: &[u8; SECTOR_SIZE]) -> bool {
    matches!(sector[0], 0xEB | 0xE9) && (&sector[54..57] == b"FAT" || &sector[82..87] == b"FAT32")
}

/// A window onto a whole disk.
pub struct Partition {
    disk: &'static dyn BlockDevice,
    name: &'static str,
    start: u64,
    sectors: u64,
}

impl Partition {
    /// A partition covering `[start, start + sectors)` of `disk`. The caller
    /// has validated the extent (see [`parse_mbr`]).
    pub fn new(
        disk: &'static dyn BlockDevice,
        name: &'static str,
        start: u64,
        sectors: u64,
    ) -> Partition {
        Partition {
            disk,
            name,
            start,
            sectors,
        }
    }

    /// Bounds-check against the partition and translate to a disk LBA.
    fn translate(&self, lba: u64, bytes: usize) -> Result<u64, BlockError> {
        check_range(self.sector_size(), self.sectors, lba, bytes)?;
        self.start.checked_add(lba).ok_or(BlockError::Bounds)
    }
}

impl BlockDevice for Partition {
    fn name(&self) -> &'static str {
        self.name
    }

    fn sector_size(&self) -> usize {
        self.disk.sector_size()
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        self.disk.read_sectors(self.translate(lba, buf.len())?, buf)
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        self.disk
            .write_sectors(self.translate(lba, buf.len())?, buf)
    }

    fn read_sectors_vectored(&self, lba: u64, bufs: &mut [&mut [u8]]) -> Result<(), BlockError> {
        let total = bufs.iter().map(|buf| buf.len()).sum();
        self.disk
            .read_sectors_vectored(self.translate(lba, total)?, bufs)
    }

    fn write_sectors_vectored(&self, lba: u64, bufs: &[&[u8]]) -> Result<(), BlockError> {
        let total = bufs.iter().map(|buf| buf.len()).sum();
        self.disk
            .write_sectors_vectored(self.translate(lba, total)?, bufs)
    }

    fn flush(&self) -> Result<(), BlockError> {
        self.disk.flush()
    }

    fn is_writable(&self) -> bool {
        self.disk.is_writable()
    }

    fn is_partition(&self) -> bool {
        true
    }
}

/// A registry name (`virtio0p3`) in static storage, so [`BlockDevice::name`]
/// can hand out a `&'static str` without a heap.
struct NameBuf {
    bytes: [u8; 16],
    len: usize,
}

impl NameBuf {
    fn new(disk: &str, index: u8) -> Option<NameBuf> {
        let disk = disk.as_bytes();
        let len = disk.len() + 2;
        let mut bytes = [0u8; 16];
        bytes.get_mut(..len)?[..disk.len()].copy_from_slice(disk);
        bytes[disk.len()] = b'p';
        bytes[disk.len() + 1] = b'0' + index;
        Some(NameBuf { bytes, len })
    }

    fn as_str(&self) -> &str {
        str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

struct Slot {
    name: Once<NameBuf>,
    partition: Once<Partition>,
}

static POOL: [Slot; MAX_PARTITIONS] = [const {
    Slot {
        name: Once::new(),
        partition: Once::new(),
    }
}; MAX_PARTITIONS];
static NEXT: spin::Mutex<usize> = spin::Mutex::new(0);
static SCANNED: AtomicBool = AtomicBool::new(false);

/// Register `entry` of `disk` from the pool. Logs and returns `None` when the
/// pool or the registry is full.
fn register_entry(
    disk: &'static dyn BlockDevice,
    entry: &Entry,
) -> Option<&'static dyn BlockDevice> {
    let mut next = NEXT.lock();
    let Some(slot) = POOL.get(*next) else {
        serial_println!(
            "block: partition pool full; {}p{} skipped",
            disk.name(),
            entry.index
        );
        return None;
    };
    *next += 1; // the slot is spent whether or not registration succeeds
    let Some(name) = NameBuf::new(disk.name(), entry.index) else {
        serial_println!("block: name of {}p{} too long", disk.name(), entry.index);
        return None;
    };
    let name = slot.name.call_once(|| name).as_str();
    let partition = slot
        .partition
        .call_once(|| Partition::new(disk, name, entry.lba, entry.sectors));
    if super::register(partition).is_err() {
        serial_println!("block: cannot register {name}");
        return None;
    }
    serial_println!(
        "block: {name} type {:#04x} lba {} sectors {}",
        entry.kind,
        entry.lba,
        entry.sectors
    );
    Some(partition)
}

/// Read the MBR of one whole disk and register its partitions as
/// `<disk>p<n>`; returns how many were registered. Call it once per disk: a
/// second scan finds the names taken and spends pool slots for nothing. A
/// partition device is refused (no nested tables).
pub fn scan_disk(disk: &'static dyn BlockDevice) -> usize {
    let mut mbr = [0u8; SECTOR_SIZE];
    if disk.is_partition()
        || disk.sector_size() != SECTOR_SIZE
        || disk.read_sectors(0, &mut mbr).is_err()
    {
        return 0;
    }
    parse_mbr(&mbr, disk.sector_count(), disk.name())
        .iter()
        .flatten()
        .filter(|entry| register_entry(disk, entry).is_some())
        .count()
}

/// Scan every registered whole disk, once. `ram0` is skipped: the ramdisk
/// registers after this runs and scans itself (`mem::register_ramdisk`).
pub fn scan_all() {
    if SCANNED.swap(true, Ordering::AcqRel) {
        return;
    }
    for disk in super::devices() {
        if !disk.is_partition() && disk.name() != super::mem::RAMDISK_NAME {
            scan_disk(disk);
        }
    }
}
