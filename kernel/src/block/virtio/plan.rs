//! Cutting a transfer into device requests (docs/performance-plan.md P5).
//!
//! The device reads and writes the caller's own buffers: each request is a
//! chain of descriptors, one per piece of a segment that stays inside one
//! virtual page (the heap and the stacks map scattered frames, so every page
//! is translated on its own). A request carries at most [`MAX_PIECES`] pieces
//! and `max_bytes` bytes and always ends on a sector boundary, so a segment
//! may straddle two requests and a request may span several segments.

use crate::block::SECTOR_SIZE;

/// Data descriptors in one request.
pub const MAX_PIECES: usize = 64;
const PAGE: u64 = 4096;

/// A position in a segment list: which segment, and how far into it.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub segment: usize,
    pub offset: usize,
}

impl Cursor {
    /// Move `bytes` forward through `segments` (`(address, length)` pairs).
    pub fn advance(&mut self, segments: &[(u64, usize)], mut bytes: usize) {
        while bytes > 0 {
            let Some(&(_, len)) = segments.get(self.segment) else {
                return;
            };
            let step = (len - self.offset).min(bytes);
            bytes -= step;
            self.offset += step;
            if self.offset == len {
                self.segment += 1;
                self.offset = 0;
            }
        }
    }
}

/// One request's data: `(physical address, length)` pieces.
pub struct Plan {
    pub pieces: [(u64, u32); MAX_PIECES],
    pub count: usize,
    pub bytes: usize,
}

/// Why no request could be planned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// A page of the buffer has no physical address.
    Unmapped,
    /// The pieces that fit hold less than one sector.
    Tiny,
}

/// The next request from `at`: up to [`MAX_PIECES`] page-bounded pieces and
/// `max_bytes` (a sector multiple) bytes, trimmed back to a sector boundary.
/// `translate` maps a virtual address to its physical one.
pub fn plan(
    segments: &[(u64, usize)],
    at: Cursor,
    max_bytes: usize,
    translate: impl Fn(u64) -> Option<u64>,
) -> Result<Plan, PlanError> {
    let mut plan = Plan {
        pieces: [(0, 0); MAX_PIECES],
        count: 0,
        bytes: 0,
    };
    let mut cursor = at;
    while plan.count < MAX_PIECES && plan.bytes < max_bytes {
        let Some(&(base, len)) = segments.get(cursor.segment) else {
            break;
        };
        if cursor.offset >= len {
            cursor.segment += 1;
            cursor.offset = 0;
            continue;
        }
        let virt = base + cursor.offset as u64;
        let to_page_end = (PAGE - virt % PAGE) as usize;
        let take = to_page_end
            .min(len - cursor.offset)
            .min(max_bytes - plan.bytes);
        let phys = translate(virt).ok_or(PlanError::Unmapped)?;
        plan.pieces[plan.count] = (phys, take as u32);
        plan.count += 1;
        plan.bytes += take;
        cursor.offset += take;
    }
    // End on a sector: give back the tail beyond the last whole sector.
    let mut excess = plan.bytes % SECTOR_SIZE;
    while excess > 0 {
        let last = &mut plan.pieces[plan.count - 1];
        let cut = (last.1 as usize).min(excess);
        last.1 -= cut as u32;
        excess -= cut;
        plan.bytes -= cut;
        if last.1 == 0 {
            plan.count -= 1;
        }
    }
    if plan.bytes == 0 {
        return Err(PlanError::Tiny);
    }
    Ok(plan)
}
