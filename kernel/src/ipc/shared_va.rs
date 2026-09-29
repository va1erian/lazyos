//! Virtual-address allocator for shared-buffer mappings.
//!
//! Ranges come from a bump cursor; a released range goes back on a sorted,
//! coalesced free list and is reused first-fit, so a create/close loop touches
//! a bounded set of addresses (and therefore a bounded set of page-table
//! frames) instead of marching the cursor forever (issue #237).

use alloc::vec::Vec;

use spin::Mutex;

/// Mapping granule.
const FRAME_SIZE: u64 = 4096;

/// Base of the virtual range buffer mappings are handed out from. It sits in
/// the lower (user) canonical half, above the kernel heap and far above every
/// program segment, stack and `mmap` bump, so a fresh mapping never collides
/// with an existing one.
pub const BUFFER_VA_BASE: u64 = 0x0000_5000_0000_0000;

/// A free `[start, start + len)` range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: u64,
    len: u64,
}

struct Allocator {
    next: u64,
    free: Vec<Span>,
}

impl Allocator {
    const fn new() -> Self {
        Self {
            next: BUFFER_VA_BASE,
            free: Vec::new(),
        }
    }

    fn alloc(&mut self, len: u64) -> u64 {
        if let Some(index) = self.free.iter().position(|span| span.len >= len) {
            let span = &mut self.free[index];
            let start = span.start;
            span.start += len;
            span.len -= len;
            if span.len == 0 {
                self.free.remove(index);
            }
            return start;
        }
        let start = self.next;
        self.next += len;
        start
    }

    fn release(&mut self, start: u64, len: u64) {
        let at = self.free.partition_point(|span| span.start < start);
        self.free.insert(at, Span { start, len });
        // Merge with the following, then the preceding, neighbour.
        if at + 1 < self.free.len()
            && self.free[at].start + self.free[at].len == self.free[at + 1].start
        {
            self.free[at].len += self.free[at + 1].len;
            self.free.remove(at + 1);
        }
        if at > 0 && self.free[at - 1].start + self.free[at - 1].len == self.free[at].start {
            self.free[at - 1].len += self.free[at].len;
            self.free.remove(at);
        }
        // A free range that touches the cursor gives it back.
        if let Some(last) = self.free.last().copied() {
            if last.start + last.len == self.next {
                self.next = last.start;
                self.free.pop();
            }
        }
    }
}

static ALLOCATOR: Mutex<Allocator> = Mutex::new(Allocator::new());

/// Hand out a virtual range for `pages` pages. Never overlaps a range that
/// has not been released.
pub fn alloc(pages: u64) -> u64 {
    ALLOCATOR.lock().alloc(pages * FRAME_SIZE)
}

/// Return the range `alloc` handed out for `pages` pages at `va`. Call only
/// once every mapping in the range is gone.
pub fn release(va: u64, pages: u64) {
    ALLOCATOR.lock().release(va, pages * FRAME_SIZE);
}

/// `(cursor, free ranges)` for tests: proves reuse and coalescing.
#[cfg(lazyos_tests)]
pub fn snapshot() -> (u64, usize) {
    let allocator = ALLOCATOR.lock();
    (allocator.next, allocator.free.len())
}
