//! Physical frame allocation from the bootloader memory map.
//!
//! This is a simple bump allocator: it hands out 4 KiB frames from the usable
//! regions in order. It needs no heap itself, which is important because it is
//! what bootstraps the kernel heap.

use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};
use x86_64::PhysAddr;

/// Lowest physical address we hand out (skip the first 1 MiB of legacy memory).
const LOWEST_FRAME: u64 = 0x10_0000;

pub struct BumpFrameAllocator<'a> {
    regions: &'a MemoryRegions,
    cursor: u64,
    allocated: usize,
    total: usize,
}

impl<'a> BumpFrameAllocator<'a> {
    pub fn new(regions: &'a MemoryRegions) -> Self {
        let total = regions
            .iter()
            .filter(|region| region.kind == MemoryRegionKind::Usable)
            .map(|region| ((region.end - region.start) / 4096) as usize)
            .sum();
        BumpFrameAllocator {
            regions,
            cursor: LOWEST_FRAME,
            allocated: 0,
            total,
        }
    }

    pub fn allocated(&self) -> usize {
        self.allocated
    }

    pub fn total(&self) -> usize {
        self.total
    }
}

unsafe impl FrameAllocator<Size4KiB> for BumpFrameAllocator<'_> {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        for region in self.regions.iter() {
            if region.kind != MemoryRegionKind::Usable {
                continue;
            }
            let start = region.start.max(self.cursor);
            let aligned = (start + 0xFFF) & !0xFFF;
            if aligned + 4096 <= region.end {
                self.cursor = aligned + 4096;
                self.allocated += 1;
                return Some(PhysFrame::containing_address(PhysAddr::new(aligned)));
            }
        }
        None
    }
}
