//! The usable physical regions, gathered from the firmware memory map.
//!
//! Real firmware maps are messy: UEFI splits RAM into dozens of entries
//! (boot-services code and data, ACPI reclaim...), entries can arrive out of
//! order, touch or overlap, and RAM above the 4 GiB PCI hole is a separate
//! entry. [`Regions::gather`] turns any such list into a short, sorted,
//! merged, page-aligned set: adjacent and overlapping usable ranges become
//! one region, so a 60-entry map usually needs a handful of slots. If a map
//! still has more disjoint ranges than [`MAX_REGIONS`], the smallest are
//! dropped and counted in [`Regions::dropped`], never the large ones.
//!
//! Pure (no allocation, no globals), so the suite can feed it synthetic maps.

use super::frames::{FRAME_SIZE, LOWEST_FRAME};

/// Maximum disjoint usable regions tracked (no heap needed to bootstrap).
pub const MAX_REGIONS: usize = 128;

/// A sorted set of disjoint, frame-aligned usable regions.
#[derive(Clone, Copy)]
pub struct Regions {
    pub starts: [u64; MAX_REGIONS],
    pub ends: [u64; MAX_REGIONS],
    pub count: usize,
    /// Bytes of usable memory left out because the set was full.
    pub dropped: u64,
}

impl Regions {
    pub const fn empty() -> Regions {
        Regions {
            starts: [0; MAX_REGIONS],
            ends: [0; MAX_REGIONS],
            count: 0,
            dropped: 0,
        }
    }

    /// Gather `(start, end)` usable ranges (end exclusive, any order, any
    /// alignment). Memory below [`LOWEST_FRAME`] is never used.
    pub fn gather(ranges: impl IntoIterator<Item = (u64, u64)>) -> Regions {
        let mut regions = Regions::empty();
        for (start, end) in ranges {
            let start = start.max(LOWEST_FRAME).saturating_add(FRAME_SIZE - 1) & !(FRAME_SIZE - 1);
            let end = end & !(FRAME_SIZE - 1);
            if start < end {
                regions.insert(start, end);
            }
        }
        regions
    }

    /// Total bytes in the set.
    pub fn bytes(&self) -> u64 {
        (0..self.count).map(|i| self.ends[i] - self.starts[i]).sum()
    }

    /// One past the highest usable byte (0 for an empty set).
    pub fn highest(&self) -> u64 {
        if self.count == 0 {
            0
        } else {
            self.ends[self.count - 1]
        }
    }

    /// Add `[start, end)`, merging with every region it overlaps or touches.
    fn insert(&mut self, mut start: u64, mut end: u64) {
        // First region that ends at or after `start` (could merge or follow).
        let mut at = (0..self.count)
            .find(|&i| self.ends[i] >= start)
            .unwrap_or(self.count);
        // Swallow every region the new range overlaps or touches.
        while at < self.count && self.starts[at] <= end {
            start = start.min(self.starts[at]);
            end = end.max(self.ends[at]);
            self.remove(at);
        }
        if self.count == MAX_REGIONS {
            // Full: keep the larger of the new range and the smallest region.
            let smallest = (0..self.count)
                .min_by_key(|&i| self.ends[i] - self.starts[i])
                .unwrap_or(0);
            let small = self.ends[smallest] - self.starts[smallest];
            if end - start <= small {
                self.dropped += end - start;
                return;
            }
            self.dropped += small;
            self.remove(smallest);
            if smallest < at {
                at -= 1;
            }
        }
        for i in (at..self.count).rev() {
            self.starts[i + 1] = self.starts[i];
            self.ends[i + 1] = self.ends[i];
        }
        self.starts[at] = start;
        self.ends[at] = end;
        self.count += 1;
    }

    fn remove(&mut self, index: usize) {
        for i in index..self.count - 1 {
            self.starts[i] = self.starts[i + 1];
            self.ends[i] = self.ends[i + 1];
        }
        self.count -= 1;
    }
}
