//! virtio-blk request path: chained multi-page requests (docs/filesystem-plan.md
//! F1). Runs against a 16 MiB scratch virtio disk that `tools/test/run.py`
//! attaches next to the boot disk; without one the tests report a skip.

use super::*;
use crate::block::{self, BlockDevice, SECTOR_SIZE};
use alloc::vec;

/// Capacity of the scratch disk the runner attaches (16 MiB).
const SCRATCH_SECTORS: u64 = 32 * 1024;
/// The window the tests fill and verify: 8 MiB, starting at an odd sector.
const WINDOW_SECTORS: u64 = 16 * 1024;
const WINDOW_START: u64 = 3;
/// Request sizes in sectors, from one sector to 64 KiB + 1 sector.
const SIZES: [usize; 14] = [1, 2, 7, 8, 9, 15, 16, 17, 63, 64, 65, 127, 128, 129];

fn scratch(test: &str) -> Option<&'static dyn BlockDevice> {
    let boot = block::boot_device().map(|device| device.name());
    let found = block::devices().into_iter().find(|device| {
        device.name().starts_with("virtio")
            && Some(device.name()) != boot
            && device.sector_count() == SCRATCH_SECTORS
    });
    if found.is_none() {
        serial_println!("TEST:{test}:INFO:no scratch virtio disk; skipped");
    }
    found
}

/// The byte every position of the window holds: a function of the absolute
/// offset, so the test needs no reference buffer.
fn pattern(offset: u64) -> u8 {
    (offset.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8 ^ (offset >> 9) as u8
}

fn fill(buf: &mut [u8], first_sector: u64) {
    for (index, byte) in buf.iter_mut().enumerate() {
        *byte = pattern(first_sector * SECTOR_SIZE as u64 + index as u64);
    }
}

/// Write 8 MiB as requests of every size at unaligned positions, then read it
/// back with a different size sequence and compare every byte.
pub fn virtio_roundtrip_unaligned() -> Result<(), String> {
    let Some(disk) = scratch("virtio_roundtrip_unaligned") else {
        return Ok(());
    };
    let end = WINDOW_START + WINDOW_SECTORS;
    let mut buf = vec![0u8; 129 * SECTOR_SIZE];

    let (mut lba, mut turn) = (WINDOW_START, 0usize);
    while lba < end {
        let sectors = SIZES[turn % SIZES.len()].min((end - lba) as usize);
        let bytes = sectors * SECTOR_SIZE;
        fill(&mut buf[..bytes], lba);
        disk.write_sectors(lba, &buf[..bytes])
            .map_err(|e| format!("write {lba}+{sectors}: {e:?}"))?;
        lba += sectors as u64;
        turn += 1;
    }

    // Read back in a different rhythm so no request lines up with a write.
    let (mut lba, mut turn) = (WINDOW_START, 5usize);
    while lba < end {
        let sectors = SIZES[turn % SIZES.len()].min((end - lba) as usize);
        let bytes = sectors * SECTOR_SIZE;
        buf[..bytes].fill(0xEE);
        disk.read_sectors(lba, &mut buf[..bytes])
            .map_err(|e| format!("read {lba}+{sectors}: {e:?}"))?;
        for (index, byte) in buf[..bytes].iter().enumerate() {
            let want = pattern(lba * SECTOR_SIZE as u64 + index as u64);
            check!(
                *byte == want,
                "lba {lba}+{sectors} byte {index}: {byte:#x}, want {want:#x}"
            );
        }
        lba += sectors as u64;
        turn += 1;
    }

    // Neither neighbour of the window was written.
    let mut edge = [0u8; SECTOR_SIZE];
    for lba in [0, WINDOW_START - 1, end] {
        disk.read_sectors(lba, &mut edge)
            .map_err(|e| format!("edge {lba}: {e:?}"))?;
        check!(
            edge.iter().all(|b| *b == 0),
            "sector {lba} outside the window was written"
        );
    }
    check!(
        disk.read_sectors(SCRATCH_SECTORS, &mut edge).is_err(),
        "read past the end"
    );
    Ok(())
}

/// Sequential 64 KiB reads of the window, reported as KiB per giga-cycle. The `INFO` line is
/// what the PR compares before and after.
pub fn virtio_read_throughput() -> Result<(), String> {
    let Some(disk) = scratch("virtio_read_throughput") else {
        return Ok(());
    };
    let mut buf = vec![0u8; 128 * SECTOR_SIZE];
    let start = crate::boot_trace::tsc();
    let mut lba = WINDOW_START;
    while lba + 128 <= WINDOW_START + WINDOW_SECTORS {
        disk.read_sectors(lba, &mut buf)
            .map_err(|e| format!("read {lba}: {e:?}"))?;
        lba += 128;
    }
    let cycles = crate::boot_trace::tsc().wrapping_sub(start).max(1);
    let kib = (lba - WINDOW_START) * SECTOR_SIZE as u64 / 1024;
    // Tests run with interrupts off, so the tick counter does not move; the
    // TSC does. Cycles are host-relative, which is all a before/after needs.
    serial_println!(
        "TEST:virtio_read_throughput:INFO:{} KiB/Gcycle",
        kib * 1_000_000_000 / cycles
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("virtio_roundtrip_unaligned", virtio_roundtrip_unaligned),
    ("virtio_read_throughput", virtio_read_throughput),
];
