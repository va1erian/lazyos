//! `MemDisk`, the ramdisk block device, and mounting a bare FAT image from it
//! (issue #5): the OS must work from the bootloader ramdisk when no disk is
//! attached.

use super::*;
use crate::block::mem::{register_ramdisk, MemDisk, RAMDISK_NAME};
use crate::block::{self, BlockDevice, BlockError, SECTOR_SIZE};
use crate::fs::vfs::Id;
use alloc::boxed::Box;

/// A leaked zeroed region of `sectors` sectors, as the bootloader hands one over.
fn region(sectors: usize) -> &'static mut [u8] {
    Box::leak(vec![0u8; sectors * SECTOR_SIZE].into_boxed_slice())
}

/// A 32-sector FAT12 volume with no MBR (sector 0 is the BPB) holding one
/// file, `HELLO.TXT` = "hello". Layout: BPB, one FAT, a 16-entry root
/// directory (one sector), then the data area.
fn bare_fat12() -> &'static mut [u8] {
    let image = region(32);
    let put16 =
        |image: &mut [u8], at: usize, v: u16| image[at..at + 2].copy_from_slice(&v.to_le_bytes());
    image[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    put16(image, 11, 512); // bytes per sector
    image[13] = 1; // sectors per cluster
    put16(image, 14, 1); // reserved sectors
    image[16] = 1; // FAT count
    put16(image, 17, 16); // root entries
    put16(image, 19, 32); // total sectors
    image[21] = 0xF8; // media
    put16(image, 22, 1); // sectors per FAT
    image[510] = 0x55;
    image[511] = 0xAA;
    // FAT12: entries 0, 1 reserved; entry 2 (the file's only cluster) is EOF.
    image[512..517].copy_from_slice(&[0xF8, 0xFF, 0xFF, 0xFF, 0x0F]);
    // Root directory (sector 2): one 8.3 entry.
    let dir = 2 * SECTOR_SIZE;
    image[dir..dir + 11].copy_from_slice(b"HELLO   TXT");
    image[dir + 11] = 0x20; // archive
    put16(image, dir + 26, 2); // first cluster
    image[dir + 28..dir + 32].copy_from_slice(&5u32.to_le_bytes()); // size
                                                                    // Data area starts at sector 3 (cluster 2).
    image[3 * SECTOR_SIZE..3 * SECTOR_SIZE + 5].copy_from_slice(b"hello");
    image
}

/// The `BlockDevice` contract on memory: geometry, round trip, bounds, and
/// whole-sector-only lengths; a trailing partial sector is not addressable.
pub fn memdisk_contract() -> Result<(), String> {
    let disk = MemDisk::new(
        "test-mem-contract",
        Box::leak(vec![0u8; 8 * SECTOR_SIZE + 100].into_boxed_slice()),
    );
    check!(
        disk.sector_count() == 8,
        "geometry {} sectors, expected 8",
        disk.sector_count()
    );
    check!(disk.is_writable(), "a memory disk must be writable");
    let pattern: Vec<u8> = (0..2 * SECTOR_SIZE).map(|i| (i % 251) as u8).collect();
    disk.write_sectors(3, &pattern)
        .map_err(|e| format!("write: {e:?}"))?;
    let mut back = vec![0u8; 2 * SECTOR_SIZE];
    disk.read_sectors(3, &mut back)
        .map_err(|e| format!("read: {e:?}"))?;
    check!(back == pattern, "read-back differs from what was written");
    let mut sector = vec![0xFFu8; SECTOR_SIZE];
    disk.read_sectors(2, &mut sector)
        .map_err(|e| format!("read: {e:?}"))?;
    check!(
        sector.iter().all(|b| *b == 0),
        "a write spilled into the previous sector"
    );
    check!(
        disk.read_sectors(7, &mut back) == Err(BlockError::Bounds),
        "a read running off the end was accepted"
    );
    check!(
        disk.write_sectors(8, &sector) == Err(BlockError::Bounds),
        "a write past the end was accepted"
    );
    check!(
        disk.read_sectors(0, &mut back[..100]) == Err(BlockError::Unsupported),
        "a partial-sector read was accepted"
    );
    check!(
        disk.read_sectors(u64::MAX, &mut sector) == Err(BlockError::Bounds),
        "an overflowing lba was accepted"
    );
    check!(disk.flush().is_ok(), "flush failed");
    Ok(())
}

/// The bootloader hand-off refuses nonsense and registers a real region once.
pub fn ramdisk_registration_is_validated() -> Result<(), String> {
    check!(
        !register_ramdisk(0, 4096),
        "a null ramdisk address was accepted"
    );
    let tiny = region(1);
    check!(
        !register_ramdisk(tiny.as_ptr() as u64, 100),
        "a sub-sector ramdisk was accepted"
    );
    check!(
        !register_ramdisk(tiny.as_ptr() as u64, 0),
        "an empty ramdisk was accepted"
    );
    let image = bare_fat12();
    check!(
        register_ramdisk(image.as_ptr() as u64, image.len() as u64),
        "a valid ramdisk was refused"
    );
    let device = block::device(RAMDISK_NAME).ok_or("ram0 is not registered")?;
    check!(
        device.sector_count() == 32,
        "ram0 has {} sectors",
        device.sector_count()
    );
    check!(
        !register_ramdisk(image.as_ptr() as u64, image.len() as u64),
        "a second ramdisk registered under the same name"
    );
    Ok(())
}

/// A bare (MBR-less) FAT image on a memory disk mounts and reads through the
/// normal VFS: the "works from the ramdisk when no disk is attached" path.
pub fn ramdisk_mounts_bare_fat() -> Result<(), String> {
    crate::fs::init();
    let disk: &'static MemDisk = Box::leak(Box::new(MemDisk::new("test-mem-fat", bare_fat12())));
    block::register(disk).map_err(|e| format!("register: {e:?}"))?;
    let previous = block::boot_device();
    block::set_boot_device(disk);
    let outcome = (|| -> Result<(), String> {
        crate::fs::mount_device("/ramfat", "test-mem-fat").map_err(|e| format!("mount: {e:?}"))?;
        let id = Id::current();
        let data =
            crate::fs::vfs_read(id, "/ramfat/HELLO.TXT").map_err(|e| format!("read: {e:?}"))?;
        check!(
            data == b"hello",
            "read {:?}",
            String::from_utf8_lossy(&data)
        );
        let names: Vec<_> = crate::fs::vfs_readdir(id, "/ramfat")
            .map_err(|e| format!("readdir: {e:?}"))?
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        check!(names == ["HELLO.TXT"], "listing: {names:?}");
        check!(
            crate::fs::vfs_read(id, "/ramfat/MISSING.TXT").is_err(),
            "a missing file was found"
        );
        Ok(())
    })();
    // The FAT reader reads through the global boot device, so restore it
    // before any later test touches the real disk.
    if let Some(device) = previous {
        block::set_boot_device(device);
    }
    outcome
}

/// Soak: 30 000 random multi-sector writes and reads checked against a model.
pub fn soak_memdisk_random_io() -> Result<(), String> {
    const SECTORS: usize = 64;
    let disk = MemDisk::new("test-mem-soak", region(SECTORS));
    let mut model = vec![0u8; SECTORS * SECTOR_SIZE];
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..30_000u32 {
        let lba = (next() % SECTORS as u64) as usize;
        let count = 1 + (next() % 4) as usize;
        let count = count.min(SECTORS - lba);
        let range = lba * SECTOR_SIZE..(lba + count) * SECTOR_SIZE;
        if next() % 2 == 0 {
            let fill = next() as u8;
            let data: Vec<u8> = (0..range.len())
                .map(|i| fill.wrapping_add(i as u8))
                .collect();
            disk.write_sectors(lba as u64, &data)
                .map_err(|e| format!("round {round}: write {e:?}"))?;
            model[range].copy_from_slice(&data);
        } else {
            let mut got = vec![0u8; range.len()];
            disk.read_sectors(lba as u64, &mut got)
                .map_err(|e| format!("round {round}: read {e:?}"))?;
            check!(
                got == model[range],
                "round {round}: sectors {lba}+{count} differ from the model"
            );
        }
    }
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("block_memdisk_contract", memdisk_contract),
    (
        "block_ramdisk_registration_validated",
        ramdisk_registration_is_validated,
    ),
    ("block_ramdisk_mounts_bare_fat", ramdisk_mounts_bare_fat),
    ("block_soak_memdisk_random_io", soak_memdisk_random_io),
];
