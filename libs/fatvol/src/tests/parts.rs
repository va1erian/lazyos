//! Where volumes are: the whole disk, MBR partitions, GPT partitions, and
//! tables that lie.

use std::rc::Rc;

use fatfs::{format_volume, FatType, FormatVolumeOptions};
use fused::daemon::FuseFs;
use gpt::guid::{BASIC_DATA, LINUX_DATA};

use super::*;
use crate::io::{BlockIo, Cache, Shared, SECTOR};
use crate::mem::MemDisk;
use crate::parts::{candidates, Slice, Window};
use crate::{OpenError, Volume};

const DISK: u64 = 300_000;

/// Format a FAT volume of `fat` bits over `window` of `disk`.
fn format_in(disk: &MemDisk, window: Window, fat: u8) {
    let slice = Slice::new(disk.clone(), window);
    let mut cache = Cache::new(slice, true);
    cache.rewind();
    let shared = Shared(Rc::new(core::cell::RefCell::new(cache)));
    let kind = if fat == 32 {
        FatType::Fat32
    } else {
        FatType::Fat16
    };
    let options = FormatVolumeOptions::new()
        .fat_type(kind)
        .volume_label(*b"INPART     ");
    format_volume(&mut shared.clone(), options).expect("format");
    shared.0.borrow_mut().flush_dirty().expect("flush");
}

fn mbr(disk: &MemDisk, slot: usize, kind: u8, start: u32, sectors: u32) {
    let mut sector = [0u8; SECTOR];
    let mut dev = disk.clone();
    dev.read(0, &mut sector).unwrap();
    let at = 446 + slot * 16;
    sector[at + 4] = kind;
    sector[at + 8..at + 12].copy_from_slice(&start.to_le_bytes());
    sector[at + 12..at + 16].copy_from_slice(&sectors.to_le_bytes());
    sector[510] = 0x55;
    sector[511] = 0xAA;
    dev.write(0, &sector).unwrap();
}

fn open_window(disk: &MemDisk, window: Window) -> Result<Volume<Slice<MemDisk>>, OpenError> {
    Volume::open(Slice::new(disk.clone(), window), opts(), Clock::new(NOW))
}

#[test]
fn a_bare_volume_is_one_window_over_the_disk() {
    let disk = MemDisk::formatted(SECTORS, 16, LABEL);
    let mut dev = disk.clone();
    let found = candidates(&mut dev).unwrap();
    assert_eq!(
        found,
        [Window {
            start: 0,
            sectors: SECTORS
        }]
    );
}

#[test]
fn mbr_partitions_are_found_and_opened() {
    let disk = MemDisk::new(DISK);
    // A Linux partition (skipped), then FAT16 and FAT32 ones.
    mbr(&disk, 0, 0x83, 2048, 4096);
    mbr(&disk, 1, 0x06, 8192, 20_480);
    mbr(&disk, 2, 0x0C, 40_960, 131_072);
    format_in(
        &disk,
        Window {
            start: 8192,
            sectors: 20_480,
        },
        16,
    );
    format_in(
        &disk,
        Window {
            start: 40_960,
            sectors: 131_072,
        },
        32,
    );
    let mut dev = disk.clone();
    let found = candidates(&mut dev).unwrap();
    assert_eq!(found.len(), 2, "{found:?}");
    let first = open_window(&disk, found[0]).unwrap();
    assert_eq!((first.label(), first.fat_bits()), ("INPART", 16));
    let second = open_window(&disk, found[1]).unwrap();
    assert_eq!(second.fat_bits(), 32);
}

#[test]
fn a_window_writes_only_inside_itself() {
    let disk = MemDisk::new(DISK);
    mbr(&disk, 0, 0x06, 4096, 20_480);
    format_in(
        &disk,
        Window {
            start: 4096,
            sectors: 20_480,
        },
        16,
    );
    let before = disk.image();
    let mut vol = open_window(
        &disk,
        Window {
            start: 4096,
            sectors: 20_480,
        },
    )
    .unwrap();
    vol.create("w.bin", 0o644, 0, 0).unwrap();
    vol.write(fused::daemon::Target::Path("w.bin"), 0, &pattern(300_000, 4))
        .unwrap();
    vol.settle().unwrap();
    let after = disk.image();
    let (lo, hi) = (4096 * SECTOR, (4096 + 20_480) * SECTOR);
    assert_eq!(before[..lo], after[..lo], "wrote before the window");
    assert_eq!(before[hi..], after[hi..], "wrote after the window");
    assert_ne!(before[lo..hi], after[lo..hi]);
}

#[test]
fn gpt_partitions_are_found() {
    let disk = MemDisk::new(DISK);
    let mut image = disk.image();
    let layout = gpt::write::build(
        DISK,
        gpt::Guid([7; 16]),
        &[
            gpt::write::Spec {
                type_guid: LINUX_DATA,
                unique_guid: gpt::Guid([1; 16]),
                first_lba: 2048,
                last_lba: 6143,
                name: "linux",
            },
            gpt::write::Spec {
                type_guid: BASIC_DATA,
                unique_guid: gpt::Guid([2; 16]),
                first_lba: 8192,
                last_lba: 28_671,
                name: "data",
            },
        ],
    )
    .unwrap();
    layout.apply(&mut image);
    let disk = MemDisk::from_image(&image);
    format_in(
        &disk,
        Window {
            start: 8192,
            sectors: 20_480,
        },
        16,
    );
    let mut dev = disk.clone();
    let found = candidates(&mut dev).unwrap();
    // Every GPT entry is a candidate; the boot sector decides.
    assert_eq!(found.len(), 2);
    let results: Vec<_> = found.iter().map(|&w| open_window(&disk, w).is_ok()).collect();
    assert_eq!(results, [false, true]);
}

#[test]
fn a_lying_table_offers_nothing_outside_the_disk() {
    let disk = MemDisk::new(1024);
    mbr(&disk, 0, 0x0C, 100, 5000); // runs past the end
    mbr(&disk, 1, 0x0C, 0, 100); // starts at the MBR itself
    mbr(&disk, 2, 0x0C, u32::MAX - 5, 100); // wraps
    mbr(&disk, 3, 0x0C, 200, 0); // empty
    let mut dev = disk.clone();
    assert!(candidates(&mut dev).unwrap().is_empty());
    // An unsigned sector 0 offers nothing, and neither does a short disk.
    let blank = MemDisk::new(8);
    let mut dev = blank.clone();
    assert!(candidates(&mut dev).unwrap().is_empty());
}
