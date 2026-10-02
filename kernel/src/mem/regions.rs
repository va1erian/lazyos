//! The usable-RAM map the frame allocator is built from (H1 of
//! `docs/real-pc-boot-plan.md`).
//!
//! The bootloader passes the firmware's memory map through almost verbatim. A
//! UEFI map of a 32 GiB desktop has 60 to 150 descriptors, and once boot
//! services exit many neighbouring ones (conventional memory, boot-services
//! code and data, loader data) are all usable, so the raw list holds dozens of
//! usable regions that touch each other. `mem::init` used to keep the first 32
//! and silently stop, which on such a machine loses most of the RAM above
//! 4 GiB. [`UsableMap::collect`] instead sorts and coalesces touching or
//! overlapping usable regions, keeps up to [`MAX_REGIONS`] ranges, and, only if
//! a hostile map still has more disjoint ranges than that, drops the smallest
//! ones and *counts* what it dropped so boot can say so.
//!
//! Everything here is pure arithmetic over `(start, end)` pairs, so the kernel
//! suite can feed it synthetic maps (`mem_suite::regions`).

use super::{FRAME_SIZE, LOWEST_FRAME, MAX_REGIONS};

/// Highest physical address the allocator will track. Architectural x86_64
/// physical addresses have at most 52 bits, and `PhysAddr::new` refuses
/// anything above; a firmware map claiming RAM up there is lying.
pub const PHYS_LIMIT: u64 = 1 << 52;

/// Disjoint, sorted, non-touching usable ranges plus what had to be left out.
#[derive(Clone, Copy)]
pub struct UsableMap {
    pub starts: [u64; MAX_REGIONS],
    pub ends: [u64; MAX_REGIONS],
    pub count: usize,
    /// Usable regions the firmware reported (before coalescing).
    pub seen: usize,
    /// Whole ranges dropped because the table was full, or because the
    /// refcount table could not be placed while they were kept.
    pub dropped_ranges: usize,
    /// Bytes of usable RAM in the dropped ranges.
    pub dropped_bytes: u64,
}

impl UsableMap {
    pub const fn empty() -> Self {
        UsableMap {
            starts: [0; MAX_REGIONS],
            ends: [0; MAX_REGIONS],
            count: 0,
            seen: 0,
            dropped_ranges: 0,
            dropped_bytes: 0,
        }
    }

    /// Build the map from raw usable `(start, end)` pairs, in any order.
    /// Each is clamped above [`LOWEST_FRAME`] and below [`PHYS_LIMIT`] and
    /// trimmed to whole frames; empty or inverted pairs are ignored.
    pub fn collect(regions: impl Iterator<Item = (u64, u64)>) -> Self {
        let mut map = Self::empty();
        for (start, end) in regions {
            map.seen += 1;
            map.add(start, end);
        }
        map
    }

    /// Bytes of RAM the kept ranges cover.
    pub fn total_bytes(&self) -> u64 {
        (0..self.count).map(|i| self.ends[i] - self.starts[i]).sum()
    }

    /// One past the highest kept address ([`LOWEST_FRAME`] when empty).
    pub fn highest(&self) -> u64 {
        match self.count {
            0 => LOWEST_FRAME,
            n => self.ends[n - 1],
        }
    }

    /// Add one usable range, merging it with every range it touches.
    pub fn add(&mut self, start: u64, end: u64) {
        let start = align_up(start.max(LOWEST_FRAME));
        let end = end.min(PHYS_LIMIT) & !(FRAME_SIZE - 1);
        if end <= start {
            return;
        }
        // First kept range that ends at or after `start`: everything before it
        // lies strictly below and cannot touch.
        let first = (0..self.count)
            .find(|&i| self.ends[i] >= start)
            .unwrap_or(self.count);
        // Ranges from `first` up to `last` (exclusive) touch the new one.
        let last = (first..self.count)
            .find(|&i| self.starts[i] > end)
            .unwrap_or(self.count);
        if first < last {
            let merged_start = start.min(self.starts[first]);
            let merged_end = end.max(self.ends[last - 1]);
            self.starts[first] = merged_start;
            self.ends[first] = merged_end;
            self.remove_span(first + 1, last);
            return;
        }
        if self.count == MAX_REGIONS && !self.evict_smaller_than(end - start) {
            self.dropped_ranges += 1;
            self.dropped_bytes += end - start;
            return;
        }
        // Eviction may have shifted the slots: find the insertion point again.
        let at = (0..self.count)
            .find(|&i| self.starts[i] > start)
            .unwrap_or(self.count);
        self.starts.copy_within(at..self.count, at + 1);
        self.ends.copy_within(at..self.count, at + 1);
        self.starts[at] = start;
        self.ends[at] = end;
        self.count += 1;
    }

    /// Drop the smallest kept range if it is smaller than `len`, counting it.
    fn evict_smaller_than(&mut self, len: u64) -> bool {
        let Some(smallest) = (0..self.count).min_by_key(|&i| self.ends[i] - self.starts[i]) else {
            return false;
        };
        let size = self.ends[smallest] - self.starts[smallest];
        if size >= len {
            return false;
        }
        self.dropped_ranges += 1;
        self.dropped_bytes += size;
        self.remove_span(smallest, smallest + 1);
        true
    }

    /// Remove ranges `from..to`, closing the gap.
    fn remove_span(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        self.starts.copy_within(to..self.count, from);
        self.ends.copy_within(to..self.count, from);
        self.count -= to - from;
    }

    /// Drop the highest range (counted). Used when the refcount table cannot
    /// be placed: a bogus far-away range inflates the table past any region.
    pub fn drop_highest(&mut self) {
        if self.count == 0 {
            return;
        }
        self.count -= 1;
        self.dropped_ranges += 1;
        self.dropped_bytes += self.ends[self.count] - self.starts[self.count];
    }

    /// Choose where the frame refcount table goes: `(phys, frames)`, with one
    /// `u32` per frame up to [`highest`](Self::highest). When no kept range
    /// can hold it, the highest range is dropped (it is what makes the table
    /// big) and placement is retried, so a map with one absurd range still
    /// boots with the rest. `None` only when nothing is left.
    pub fn place_refcounts(&mut self) -> Option<(u64, usize)> {
        while self.count > 0 {
            let entries = self.highest() / FRAME_SIZE;
            let bytes = entries * core::mem::size_of::<u32>() as u64;
            let frames = bytes.div_ceil(FRAME_SIZE);
            let need = frames * FRAME_SIZE;
            let fits = (0..self.count).find(|&i| self.ends[i] - self.starts[i] >= need);
            if let Some(i) = fits {
                return Some((self.starts[i], frames as usize));
            }
            self.drop_highest();
        }
        None
    }
}

fn align_up(value: u64) -> u64 {
    value.saturating_add(FRAME_SIZE - 1) & !(FRAME_SIZE - 1)
}
