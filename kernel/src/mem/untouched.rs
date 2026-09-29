//! Lazily consumed frames: the part of physical memory nobody has used yet.
//!
//! `init` used to thread every usable frame onto the free list, which writes
//! one pointer into each frame. That touches all of RAM at boot, and on a
//! hypervisor each first touch of a guest page is a host page fault: ~63k
//! frames cost about 0.4 s of a 256 MiB guest's boot. The frames' contents do
//! not matter yet, so instead each region keeps a cursor and frames are handed
//! out in address order on demand; only frames that come *back* go on the
//! (intrusive) free list.

use super::{FRAME_SIZE, MAX_REGIONS};

/// Per-region cursors over frames that were never handed out.
pub struct Untouched {
    cursor: [u64; MAX_REGIONS],
    region: usize,
}

impl Untouched {
    /// Start every region's cursor at its first frame.
    pub fn new(starts: &[u64; MAX_REGIONS]) -> Self {
        Self {
            cursor: *starts,
            region: 0,
        }
    }

    /// The next never-used frame in address order, or `None` when every
    /// region is consumed. The caller skips frames it reserved itself.
    pub fn next(&mut self, ends: &[u64; MAX_REGIONS], count: usize) -> Option<u64> {
        while self.region < count {
            let phys = self.cursor[self.region];
            if phys + FRAME_SIZE <= ends[self.region] {
                self.cursor[self.region] = phys + FRAME_SIZE;
                return Some(phys);
            }
            self.region += 1;
        }
        None
    }

    /// How many frames `start..end` holds (whole frames only).
    pub fn frames_in(start: u64, end: u64) -> usize {
        (end.saturating_sub(start) / FRAME_SIZE) as usize
    }
}
