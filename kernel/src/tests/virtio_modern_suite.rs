//! The in-kernel virtio-blk over the modern transport (issue #497, driver-plan
//! D7). `tools/test/run.py` attaches a 24 MiB modern-only scratch disk
//! (`disable-legacy=on`) next to the 16 MiB legacy one; these tests find it by
//! size, check which interface drives each, run the chained-request round trip
//! on it, and soak it through device resets (the timeout recovery path) with
//! I/O in between.

use super::virtio_suite::{roundtrip, scratch_of, SCRATCH_SECTORS};
use super::*;
use crate::block::{virtio, BlockDevice, SECTOR_SIZE};
use alloc::vec;

/// Capacity of the modern scratch disk (24 MiB).
const MODERN_SECTORS: u64 = 48 * 1024;
/// Resets the soak puts the device through.
const RESETS: u32 = 300;

fn modern(test: &str) -> Option<&'static dyn BlockDevice> {
    scratch_of(test, MODERN_SECTORS)
}

/// Each scratch disk is driven through the interface QEMU gave it: the
/// modern-only one through the modern transport, the legacy-only one through
/// its I/O window.
pub fn virtio_modern_transport_chosen() -> Result<(), String> {
    let Some(disk) = modern("virtio_modern_transport_chosen") else {
        return Ok(());
    };
    let how = virtio::transport_of(disk.name()).ok_or("the modern disk is not driven")?;
    check!(how == "modern", "{} is driven as {how}", disk.name());
    check!(
        disk.sector_count() == MODERN_SECTORS,
        "capacity {} from the device configuration, want {MODERN_SECTORS}",
        disk.sector_count()
    );
    if let Some(legacy) = scratch_of("virtio_modern_transport_chosen", SCRATCH_SECTORS) {
        let how = virtio::transport_of(legacy.name()).ok_or("the legacy disk is not driven")?;
        check!(
            how.starts_with("legacy"),
            "{} is driven as {how}",
            legacy.name()
        );
    }
    Ok(())
}

/// The chained-request round trip of `virtio_suite`, over the modern transport.
pub fn virtio_modern_roundtrip_unaligned() -> Result<(), String> {
    let Some(disk) = modern("virtio_modern_roundtrip_unaligned") else {
        return Ok(());
    };
    roundtrip(disk, MODERN_SECTORS)
}

/// Reset the modern device hundreds of times (what a timed-out request does),
/// writing and reading back a block between resets: every reset must bring
/// the queue back, no data may be lost, and no frame may leak.
pub fn virtio_modern_reset_soak() -> Result<(), String> {
    let Some(disk) = modern("virtio_modern_reset_soak") else {
        return Ok(());
    };
    let name = disk.name();
    let mut block = vec![0u8; 8 * SECTOR_SIZE];
    let mut back = vec![0u8; 8 * SECTOR_SIZE];
    let lba_of = |round: u32| u64::from(round % 1024) * 16 + 1;
    // One warm round so anything allocated on first use exists.
    check!(
        virtio::reset_for_test(name),
        "the first reset detached the device"
    );
    let frames = mem::frame_stats().live();
    for round in 0..RESETS {
        let lba = lba_of(round);
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = (round as usize ^ index.wrapping_mul(31)) as u8;
        }
        disk.write_sectors(lba, &block)
            .map_err(|e| format!("round {round}: write {lba}: {e:?}"))?;
        check!(
            virtio::reset_for_test(name),
            "round {round}: the device did not come back from a reset"
        );
        back.fill(0);
        disk.read_sectors(lba, &mut back)
            .map_err(|e| format!("round {round}: read {lba}: {e:?}"))?;
        check!(
            back == block,
            "round {round}: lba {lba} read back differently"
        );
    }
    check!(
        mem::frame_stats().live() == frames,
        "{RESETS} resets leaked {} frames",
        mem::frame_stats().live() as i64 - frames as i64
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "virtio_modern_transport_chosen",
        virtio_modern_transport_chosen,
    ),
    (
        "virtio_modern_roundtrip_unaligned",
        virtio_modern_roundtrip_unaligned,
    ),
    ("virtio_modern_reset_soak", virtio_modern_reset_soak),
];
