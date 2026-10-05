//! NVMe request path (docs/nvme-install-plan.md N1). Runs against a 16 MiB
//! scratch NVMe disk that `tools/test/run.py --nvme` attaches next to the
//! boot disk (QEMU's `-device nvme`); without one the tests report a skip.
//! Bring-up failures (a controller that never becomes ready, a fatal status,
//! a hung command) are covered by `libs/nvme`'s model-controller tests.

use super::*;
use crate::block::{self, BlockDevice, Wait, SECTOR_SIZE};
use alloc::vec;

/// Capacity of the scratch disk the runner attaches (16 MiB).
const SCRATCH_SECTORS: u64 = 32 * 1024;
/// The soak's window: 2 MiB from sector 4096.
const SOAK_START: u64 = 4096;
const SOAK_SECTORS: u64 = 4096;

fn scratch(test: &str) -> Option<&'static dyn BlockDevice> {
    let found = block::devices().into_iter().find(|device| {
        device.name().starts_with("nvme")
            && !device.is_partition()
            && device.sector_count() == SCRATCH_SECTORS
    });
    if found.is_none() {
        serial_println!("TEST:{test}:INFO:no scratch NVMe disk; skipped");
    }
    found
}

/// A byte that depends on the absolute disk offset and a generation.
fn pattern(offset: u64, generation: u8) -> u8 {
    ((offset.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8 ^ (offset >> 9) as u8)
        .wrapping_add(generation)
}

fn fill(buf: &mut [u8], lba: u64, generation: u8) {
    for (index, byte) in buf.iter_mut().enumerate() {
        *byte = pattern(lba * SECTOR_SIZE as u64 + index as u64, generation);
    }
}

fn verify(buf: &[u8], lba: u64, generation: u8, what: &str) -> Result<(), String> {
    for (index, byte) in buf.iter().enumerate() {
        let want = pattern(lba * SECTOR_SIZE as u64 + index as u64, generation);
        check!(
            *byte == want,
            "{what}: lba {lba} byte {index}: {byte:#x}, want {want:#x}"
        );
    }
    Ok(())
}

/// The scratch controller came up as a whole disk of the attached size, and
/// is writable.
pub fn nvme_attached() -> Result<(), String> {
    let Some(disk) = scratch("nvme_attached") else {
        return Ok(());
    };
    check!(disk.is_writable(), "{} is not writable", disk.name());
    check!(
        disk.sector_size() == SECTOR_SIZE,
        "sector size {}",
        disk.sector_size()
    );
    serial_println!(
        "TEST:nvme_attached:INFO:{} {} sectors",
        disk.name(),
        disk.sector_count()
    );
    Ok(())
}

/// Every PRP shape the driver builds: one page, two pages, a PRP list, a
/// start in the middle of a page, a multi-command transfer, and buffers
/// that are not dword aligned (the bounce page).
pub fn nvme_prp_shapes() -> Result<(), String> {
    let Some(disk) = scratch("nvme_prp_shapes") else {
        return Ok(());
    };
    // (offset into the allocation, sectors).
    let shapes: [(usize, usize); 10] = [
        (0, 1),
        (0, 8),
        (0, 16),
        (0, 24),
        (512, 8),
        (2048, 120),
        (4, 1),
        (1, 3),
        (3, 17),
        (64, 300),
    ];
    let mut lba = 1u64;
    for (turn, &(offset, sectors)) in shapes.iter().enumerate() {
        let bytes = sectors * SECTOR_SIZE;
        let generation = turn as u8;
        let mut out = vec![0u8; offset + bytes + 4096];
        fill(&mut out[offset..offset + bytes], lba, generation);
        disk.write_sectors(lba, &out[offset..offset + bytes])
            .map_err(|e| format!("write {offset}+{sectors}: {e:?}"))?;
        let mut back = vec![0xEEu8; offset + bytes + 4096];
        disk.read_sectors(lba, &mut back[offset..offset + bytes])
            .map_err(|e| format!("read {offset}+{sectors}: {e:?}"))?;
        verify(&back[offset..offset + bytes], lba, generation, "shape")?;
        check!(
            back[..offset]
                .iter()
                .chain(&back[offset + bytes..])
                .all(|b| *b == 0xEE),
            "shape {offset}+{sectors}: read wrote outside its buffer"
        );
        lba += sectors as u64;
    }
    Ok(())
}

/// A 64 KiB vectored writeback of sixteen 4 KiB blocks (the ext2 block
/// cache's shape), read back vectored in a different split.
pub fn nvme_vectored_writeback() -> Result<(), String> {
    let Some(disk) = scratch("nvme_vectored_writeback") else {
        return Ok(());
    };
    const LBA: u64 = 2048;
    let mut blocks: Vec<Vec<u8>> = (0..16).map(|_| vec![0u8; 4096]).collect();
    for (index, block) in blocks.iter_mut().enumerate() {
        fill(block, LBA + index as u64 * 8, 7);
    }
    let bufs: Vec<&[u8]> = blocks.iter().map(|block| block.as_slice()).collect();
    disk.write_sectors_vectored_with(LBA, &bufs, Wait::MaySleep)
        .map_err(|e| format!("vectored write: {e:?}"))?;
    let mut halves: Vec<Vec<u8>> = (0..32).map(|_| vec![0u8; 2048]).collect();
    {
        let mut bufs: Vec<&mut [u8]> = halves.iter_mut().map(|half| half.as_mut_slice()).collect();
        disk.read_sectors_vectored_with(LBA, &mut bufs, Wait::MaySleep)
            .map_err(|e| format!("vectored read: {e:?}"))?;
    }
    for (index, half) in halves.iter().enumerate() {
        verify(half, LBA + index as u64 * 4, 7, "vectored")?;
    }
    Ok(())
}

/// Flush reaches the controller (QEMU's has a volatile write cache) and is
/// counted.
pub fn nvme_flush() -> Result<(), String> {
    let Some(disk) = scratch("nvme_flush") else {
        return Ok(());
    };
    let before = disk.stats().map(|stats| stats.snapshot());
    disk.flush().map_err(|e| format!("flush: {e:?}"))?;
    let after = disk.stats().map(|stats| stats.snapshot());
    if let (Some(before), Some(after)) = (before, after) {
        check!(after.flushes > before.flushes, "flush not counted");
    }
    Ok(())
}

/// Ranges outside the disk and partial sectors are refused before the
/// controller sees them.
pub fn nvme_bounds() -> Result<(), String> {
    let Some(disk) = scratch("nvme_bounds") else {
        return Ok(());
    };
    let mut sector = [0u8; SECTOR_SIZE];
    check!(
        disk.read_sectors(SCRATCH_SECTORS, &mut sector).is_err(),
        "read past the end"
    );
    check!(
        disk.write_sectors(SCRATCH_SECTORS - 1, &[0u8; 1024])
            .is_err(),
        "write across the end"
    );
    check!(
        disk.read_sectors(0, &mut sector[..100]).is_err(),
        "partial sector"
    );
    check!(
        disk.read_sectors(0, &mut sector).is_ok(),
        "the controller stayed attached"
    );
    Ok(())
}

/// Random reads and writes of random sizes and alignments over a 2 MiB
/// window, checked against a shadow copy.
pub fn nvme_soak() -> Result<(), String> {
    let Some(disk) = scratch("nvme_soak") else {
        return Ok(());
    };
    let window = SOAK_SECTORS as usize * SECTOR_SIZE;
    let mut shadow = vec![0u8; window];
    let zero = vec![0u8; 64 * 1024];
    for lba in (0..SOAK_SECTORS).step_by(128) {
        disk.write_sectors(SOAK_START + lba, &zero)
            .map_err(|e| format!("clear {lba}: {e:?}"))?;
    }
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut buf = vec![0u8; 256 * SECTOR_SIZE + 8];
    const OPS: u32 = 1500;
    for op in 0..OPS {
        let sectors = 1 + (next() % 256) as usize;
        let lba = next() % (SOAK_SECTORS - sectors as u64 + 1);
        let offset = (next() % 8) as usize;
        let bytes = sectors * SECTOR_SIZE;
        let at = lba as usize * SECTOR_SIZE;
        let data = &mut buf[offset..offset + bytes];
        if next() % 2 == 0 {
            for byte in data.iter_mut() {
                *byte = next() as u8;
            }
            disk.write_sectors(SOAK_START + lba, data)
                .map_err(|e| format!("op {op}: write {lba}+{sectors}: {e:?}"))?;
            shadow[at..at + bytes].copy_from_slice(data);
        } else {
            disk.read_sectors(SOAK_START + lba, data)
                .map_err(|e| format!("op {op}: read {lba}+{sectors}: {e:?}"))?;
            if let Some(index) = (0..bytes).find(|&index| data[index] != shadow[at + index]) {
                return Err(format!(
                    "op {op}: read {lba}+{sectors} differs at byte {index}"
                ));
            }
        }
    }
    serial_println!("TEST:nvme_soak:INFO:{OPS} random operations matched the shadow copy");
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("nvme_attached", nvme_attached),
    ("nvme_prp_shapes", nvme_prp_shapes),
    ("nvme_vectored_writeback", nvme_vectored_writeback),
    ("nvme_flush", nvme_flush),
    ("nvme_bounds", nvme_bounds),
    ("nvme_soak", nvme_soak),
];
