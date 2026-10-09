//! Where the memory goes, in words a user knows: programs, the system, the
//! disk cache and free memory, split from one snapshot's frame counters.

use super::Snapshot;

/// Bytes in one frame.
pub const PAGE: u64 = 4096;

/// One snapshot's memory, in bytes, as four shares that add up to
/// [`MemoryUse::total`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MemoryUse {
    /// Memory held by running programs and services: their code, data,
    /// stacks, window buffers and the page tables that map them. Every live
    /// frame that is not the system's or the cache's.
    pub programs: u64,
    /// Memory the kernel keeps for itself: its heap, its object slabs and the
    /// frame allocator's own bookkeeping.
    pub system: u64,
    /// Recently used file data kept for reuse; handed back to programs when
    /// they need it.
    pub cache: u64,
    /// Memory nothing uses.
    pub free: u64,
}

impl MemoryUse {
    /// All the memory the shares cover.
    pub fn total(&self) -> u64 {
        self.programs + self.system + self.cache + self.free
    }

    /// Memory in use: programs and the system. The cache is not counted, as
    /// it is given back on demand.
    pub fn used(&self) -> u64 {
        self.programs + self.system
    }

    /// Memory a program can still get: free memory and the cache.
    pub fn available(&self) -> u64 {
        self.free + self.cache
    }
}

impl Snapshot {
    /// The snapshot's memory as [`MemoryUse`] shares.
    ///
    /// The cache, the heap's pages and the slab frames are all parts of the
    /// live frames; each is clamped to what is left, so counters read a
    /// moment apart never make a share negative or the shares overlap.
    pub fn memory_use(&self) -> MemoryUse {
        let live = self.frames_live;
        let cache = self.cache_frames.min(live);
        let kernel = (self.heap_total.div_ceil(PAGE) + self.slab_frames).min(live - cache);
        MemoryUse {
            programs: (live - cache - kernel) * PAGE,
            system: (kernel + self.frames_reserved) * PAGE,
            cache: cache * PAGE,
            free: self.frames_free * PAGE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysinfo::{decode_words, header, VERSION, WORDS};

    fn snapshot(fields: &[(usize, u64)]) -> Snapshot {
        let mut words = [0u64; WORDS];
        words[header::VERSION] = VERSION;
        for &(index, value) in fields {
            words[index] = value;
        }
        decode_words(&words).expect("decodes")
    }

    #[test]
    fn the_shares_split_the_live_frames_and_add_up() {
        let memory = snapshot(&[
            (header::FRAMES_TOTAL, 1000),
            (header::FRAMES_LIVE, 600),
            (header::FRAMES_FREE, 400),
            (header::FRAMES_RESERVED, 10),
            (header::HEAP_TOTAL, 100 * PAGE),
            (header::SLAB_FRAMES, 20),
            (header::CACHE_FRAMES, 80),
        ])
        .memory_use();
        assert_eq!(memory.cache, 80 * PAGE);
        assert_eq!(memory.system, (100 + 20 + 10) * PAGE);
        assert_eq!(memory.programs, (600 - 80 - 120) * PAGE);
        assert_eq!(memory.free, 400 * PAGE);
        assert_eq!(memory.total(), 1010 * PAGE);
        assert_eq!(memory.used(), memory.programs + memory.system);
        assert_eq!(memory.available(), (400 + 80) * PAGE);
    }

    #[test]
    fn a_partial_heap_page_counts_as_a_whole_frame() {
        let memory =
            snapshot(&[(header::FRAMES_LIVE, 10), (header::HEAP_TOTAL, PAGE + 1)]).memory_use();
        assert_eq!(memory.system, 2 * PAGE);
        assert_eq!(memory.programs, 8 * PAGE);
    }

    #[test]
    fn counters_read_apart_never_go_negative() {
        // More cache and kernel pages than live frames: clamped, not wrapped.
        let memory = snapshot(&[
            (header::FRAMES_LIVE, 50),
            (header::CACHE_FRAMES, 40),
            (header::HEAP_TOTAL, 30 * PAGE),
            (header::SLAB_FRAMES, 5),
        ])
        .memory_use();
        assert_eq!(memory.cache, 40 * PAGE);
        assert_eq!(memory.system, 10 * PAGE);
        assert_eq!(memory.programs, 0);
        let memory = snapshot(&[(header::FRAMES_LIVE, 5), (header::CACHE_FRAMES, 9)]).memory_use();
        assert_eq!((memory.cache, memory.programs), (5 * PAGE, 0));
    }

    #[test]
    fn an_empty_snapshot_is_all_zero() {
        assert_eq!(snapshot(&[]).memory_use(), MemoryUse::default());
    }
}
