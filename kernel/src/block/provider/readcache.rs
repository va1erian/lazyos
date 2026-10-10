//! A write-through read cache for a provider disk (a USB stick served by
//! `usbd`, docs/architecture/usb-storage.md).
//!
//! The ext2 volume on a stick is opened uncached on purpose
//! (`fs/mounts.rs::open_ext2`): the write-back block cache flushes from the
//! kernel task, which must never wait for `usbd`, and a stick can be pulled,
//! so its writes go straight through in the writer's context. But *reads*
//! need none of that, and uncached they cost a whole round trip to a polled
//! user-space driver each: a directory open walks several blocks, and a shell
//! that looked at its desktop folder once a second kept the stick busy for
//! ever (0.27-0.33 s per `open`, measured on a real PC).
//!
//! So the disk keeps clean pages of what it read. Nothing else about the
//! stick changes: every write still goes to the device before it returns, in
//! the order the filesystem issued it, and the pages it touches are updated
//! as it completes (a failed one is forgotten), so a page is never ahead of
//! the device. A dead disk answers `Io` before it looks here.
//!
//! # Races
//!
//! A reader that missed reads the device with no lock held and stores the
//! pages afterwards; a writer may run in between. [`ReadCache::epoch`] counts
//! write starts and ends: a reader stores only if it is unchanged, so a page
//! read before (or while) a write landed is never stored.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec;

use crate::block::SECTOR_SIZE;

/// Sectors per cached page (4 KiB: an ext2 block, a directory block).
pub const PAGE_SECTORS: u64 = 8;
const PAGE_BYTES: usize = PAGE_SECTORS as usize * SECTOR_SIZE;
/// Pages kept per disk (2 MiB): metadata and the hot files, not a streamed
/// file, which is [`BYPASS_BYTES`].
const MAX_PAGES: usize = 512;
/// A request larger than this is a stream (file data, `readahead`), not
/// metadata: it neither uses nor fills the cache.
pub const BYPASS_BYTES: usize = 32 * 1024;

/// One disk's read cache.
pub struct ReadCache {
    pages: BTreeMap<u64, Box<[u8]>>,
    /// Page numbers, oldest first (the eviction order).
    order: VecDeque<u64>,
    epoch: u64,
    hits: u64,
    misses: u64,
}

impl ReadCache {
    pub const fn new() -> ReadCache {
        ReadCache {
            pages: BTreeMap::new(),
            order: VecDeque::new(),
            epoch: 0,
            hits: 0,
            misses: 0,
        }
    }

    /// Writes started or finished so far: a reader's stamp for [`insert`].
    ///
    /// [`insert`]: ReadCache::insert
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// `(hits, misses)` of whole reads, for the tests.
    #[cfg(lazyos_tests)]
    pub fn counters(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Pages held.
    #[cfg(lazyos_tests)]
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Fill `buf` (whole sectors at `lba`) when every page it touches is
    /// held; `false` leaves it alone.
    pub fn read(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let sectors = (buf.len() / SECTOR_SIZE) as u64;
        if sectors == 0 {
            return true;
        }
        let first = lba / PAGE_SECTORS;
        let last = (lba + sectors - 1) / PAGE_SECTORS;
        if (first..=last).any(|page| !self.pages.contains_key(&page)) {
            self.misses += 1;
            return false;
        }
        for (index, sector) in buf.as_chunks_mut::<SECTOR_SIZE>().0.iter_mut().enumerate() {
            let at = lba + index as u64;
            let offset = (at % PAGE_SECTORS) as usize * SECTOR_SIZE;
            if let Some(page) = self.pages.get(&(at / PAGE_SECTORS)) {
                sector.copy_from_slice(&page[offset..offset + SECTOR_SIZE]);
            }
        }
        self.hits += 1;
        true
    }

    /// Keep the whole pages in `data` (sectors from `lba`, a page boundary),
    /// unless a write happened since `epoch` was read. A page already held
    /// stays: it is current by construction.
    pub fn insert(&mut self, epoch: u64, lba: u64, data: &[u8]) {
        if epoch != self.epoch || !lba.is_multiple_of(PAGE_SECTORS) {
            return;
        }
        for (index, bytes) in data.as_chunks::<PAGE_BYTES>().0.iter().enumerate() {
            let page = lba / PAGE_SECTORS + index as u64;
            if self.pages.contains_key(&page) {
                continue;
            }
            if self.pages.len() >= MAX_PAGES {
                if let Some(oldest) = self.order.pop_front() {
                    self.pages.remove(&oldest);
                }
            }
            let mut copy = vec![0u8; PAGE_BYTES].into_boxed_slice();
            copy.copy_from_slice(bytes);
            self.pages.insert(page, copy);
            self.order.push_back(page);
        }
    }

    /// Forget everything (a disk registering in a slot another one used).
    pub fn clear(&mut self) {
        self.epoch += 1;
        self.pages.clear();
        self.order.clear();
    }

    /// A write is about to reach the device: stamps taken before this are
    /// stale for [`insert`](ReadCache::insert). Returns the write's ticket
    /// for [`wrote`](ReadCache::wrote).
    pub fn begin_write(&mut self) -> u64 {
        self.epoch += 1;
        self.epoch
    }

    /// A write of `data` at `lba` completed: held pages it overlaps take the
    /// new bytes, but only if no other write began or ended since `ticket`
    /// (from [`begin_write`](ReadCache::begin_write)). Two overlapping
    /// writes finish on the device in an order this thread cannot see, and
    /// the later cache update could be the older bytes; with another write
    /// in between the pages are dropped instead, and the next read takes
    /// what the device holds.
    pub fn wrote(&mut self, ticket: u64, lba: u64, data: &[u8]) {
        if ticket != self.epoch {
            self.forget(lba, (data.len() / SECTOR_SIZE) as u64);
            return;
        }
        self.epoch += 1;
        for (index, sector) in data.as_chunks::<SECTOR_SIZE>().0.iter().enumerate() {
            let at = lba + index as u64;
            if let Some(page) = self.pages.get_mut(&(at / PAGE_SECTORS)) {
                let offset = (at % PAGE_SECTORS) as usize * SECTOR_SIZE;
                page[offset..offset + SECTOR_SIZE].copy_from_slice(sector);
            }
        }
    }

    /// A write failed part way: the device may hold the old or the new
    /// bytes, so drop every page it overlaps.
    pub fn forget(&mut self, lba: u64, sectors: u64) {
        self.epoch += 1;
        if sectors == 0 {
            return;
        }
        for page in lba / PAGE_SECTORS..=(lba + sectors - 1) / PAGE_SECTORS {
            if self.pages.remove(&page).is_some() {
                self.order.retain(|held| *held != page);
            }
        }
    }
}
