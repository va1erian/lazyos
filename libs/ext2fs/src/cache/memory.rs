//! Where cached blocks live, and the knobs a host sets when it opens a volume.
//!
//! The cache never allocates block memory itself: the host hands it a
//! [`CacheMemory`]. The host build and the tests use [`HeapMemory`]; the kernel
//! supplies whole physical frames so a large cache does not eat its small
//! heap. A page that is dropped returns to whoever made it.

use alloc::boxed::Box;
use alloc::vec::Vec;

/// Bytes in one cache page: the largest block size the driver supports, so
/// any volume's block fits in one page.
pub const CACHE_PAGE_SIZE: usize = super::super::layout::MAX_BLOCK_SIZE;

/// One page of cache memory, [`CACHE_PAGE_SIZE`] bytes long.
pub trait CachePage: Send {
    fn bytes(&self) -> &[u8];
    fn bytes_mut(&mut self) -> &mut [u8];
}

/// The source of cache pages. `alloc` may fail at any time (memory is
/// short): the cache then recycles the pages it already has instead.
pub trait CacheMemory: Send + Sync {
    fn alloc(&self) -> Option<Box<dyn CachePage>>;
}

/// Pages from the global allocator, for hosts with a roomy heap.
pub struct HeapMemory;

struct HeapPage(Vec<u8>);

impl CachePage for HeapPage {
    fn bytes(&self) -> &[u8] {
        &self.0
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl CacheMemory for HeapMemory {
    fn alloc(&self) -> Option<Box<dyn CachePage>> {
        let mut page = Vec::new();
        page.try_reserve_exact(CACHE_PAGE_SIZE).ok()?;
        page.resize(CACHE_PAGE_SIZE, 0);
        Some(Box::new(HeapPage(page)))
    }
}

/// How a volume caches its blocks ([`crate::Ext2::open_cached`]).
pub struct CacheConfig {
    /// Most blocks held at once; each takes one page of `memory`.
    pub blocks: usize,
    /// Dirty blocks that trigger a full ordered writeback. Bounds both the
    /// work lost to a crash and the stall a writer can meet.
    pub dirty_limit: usize,
    /// Largest single device request the writeback and read-ahead build, in
    /// bytes (a virtio-blk request carries at most 64 KiB here).
    pub max_request: usize,
    /// Blocks read ahead after a miss that continues the previous one.
    pub readahead: usize,
    pub memory: Box<dyn CacheMemory>,
}

impl CacheConfig {
    /// `blocks` heap pages, half of them allowed dirty, 64 KiB requests and
    /// 64 KiB of read-ahead: the defaults every host starts from.
    pub fn heap(blocks: usize) -> CacheConfig {
        CacheConfig::with_memory(blocks, Box::new(HeapMemory))
    }

    /// The defaults of [`CacheConfig::heap`] over another page source.
    pub fn with_memory(blocks: usize, memory: Box<dyn CacheMemory>) -> CacheConfig {
        CacheConfig {
            blocks,
            dirty_limit: (blocks / 2).max(1),
            max_request: 64 * 1024,
            readahead: 16,
            memory,
        }
    }
}
