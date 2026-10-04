//! A write-back block cache between the driver and its [`BlockIo`].
//!
//! Each cached filesystem block owns one page from the host's
//! [`CacheMemory`]. Reads are served from the cache: a miss reads the blocks
//! the caller wants in one request, plus read-ahead when it continues the
//! previous miss, and a long run of uncached blocks wanted whole goes straight
//! to the caller's buffer instead ([`range`]). Writes only dirty the page. Dirty
//! blocks reach the disk in one *writeback* ([`BlockCache::flush`]): every
//! dirty block, in the crash-safe phase order of [`roles`], each phase sorted
//! by block number and coalesced into requests of up to `max_request` bytes.
//! A writeback runs when the volume commits (sync, fsync, unmount, the
//! kernel's periodic flusher), when `dirty_limit` blocks are dirty, and when
//! a block is needed and every page is dirty. Nothing else ever writes, so
//! the phase order holds for every byte that reaches the disk.
//!
//! Eviction is CLOCK over clean blocks only: a dirty block is never dropped,
//! it is written back first. Memory is bounded by `blocks` pages, and a host
//! that cannot supply a page just gets an older one recycled.
//!
//! The cache is used under the volume lock only, so it has no locking of its
//! own beyond the `Mutex` the volume keeps it in.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::{BlockIo, IoError, SECTOR_SIZE};

mod flush;
pub mod memory;
mod range;
pub mod roles;

use memory::{CacheConfig, CacheMemory, CachePage};
use roles::Roles;

/// Counters a host can read to see what the cache saved.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    /// Blocks brought in by read-ahead beyond the ones asked for.
    pub readahead: u64,
    /// Blocks read straight into a caller's buffer, past the cache.
    pub bypassed: u64,
    /// Full writebacks, and the requests and blocks they wrote.
    pub writebacks: u64,
    pub write_requests: u64,
    pub written: u64,
    /// Clean blocks dropped to make room.
    pub evictions: u64,
    /// Blocks held now, how many of them are dirty, and pages of memory owned.
    pub cached: usize,
    pub dirty: usize,
    pub pages: usize,
}

/// One cache entry. A slot with no `block` is free (on the free list or held
/// by a read in progress); one with no `page` gave its memory back.
struct Slot {
    block: Option<u64>,
    page: Option<Box<dyn CachePage>>,
    dirty: bool,
    /// Allocated and not written back since ([`roles::Phase::Fresh`]).
    fresh: bool,
    /// CLOCK's second-chance bit.
    referenced: bool,
}

pub(crate) struct BlockCache {
    block_size: usize,
    sectors_per_block: u64,
    blocks_count: u64,
    roles: Roles,
    memory: Box<dyn CacheMemory>,
    /// Most pages held at once.
    limit: usize,
    dirty_limit: usize,
    /// Blocks per coalesced request.
    max_run: usize,
    readahead: usize,
    slots: Vec<Slot>,
    map: BTreeMap<u64, usize>,
    /// Slots with a page and no block.
    free: Vec<usize>,
    /// Slots that have given their page back.
    bare: Vec<usize>,
    pages: usize,
    dirty: usize,
    hand: usize,
    /// The last block a miss read, for spotting sequential reads.
    last_miss: Option<u64>,
    /// A writeback request failed since [`BlockCache::take_failure`].
    failed: bool,
    stats: CacheStats,
}

impl BlockCache {
    pub fn new(config: CacheConfig, block_size: u32, blocks_count: u32, roles: Roles) -> Self {
        let block_size = block_size as usize;
        BlockCache {
            block_size,
            sectors_per_block: (block_size / SECTOR_SIZE) as u64,
            blocks_count: u64::from(blocks_count),
            roles,
            memory: config.memory,
            limit: config.blocks.max(1),
            dirty_limit: config.dirty_limit.clamp(1, config.blocks.max(1)),
            max_run: (config.max_request / block_size).max(1),
            readahead: config.readahead,
            slots: Vec::new(),
            map: BTreeMap::new(),
            free: Vec::new(),
            bare: Vec::new(),
            pages: 0,
            dirty: 0,
            hand: 0,
            last_miss: None,
            failed: false,
            stats: CacheStats::default(),
        }
    }

    /// Whether a writeback request failed since the last call. A failed read
    /// is the caller's error alone; a failed writeback is the volume's.
    pub fn take_failure(&mut self) -> bool {
        core::mem::take(&mut self.failed)
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            cached: self.map.len(),
            dirty: self.dirty,
            pages: self.pages,
            ..self.stats
        }
    }

    pub fn dirty_blocks(&self) -> usize {
        self.dirty
    }

    /// Copy `block` into `buf` (one block long), reading it on a miss.
    pub fn read(&mut self, io: &dyn BlockIo, block: u64, buf: &mut [u8]) -> Result<(), IoError> {
        if let Some(&index) = self.map.get(&block) {
            self.stats.hits += 1;
            let slot = &mut self.slots[index];
            slot.referenced = true;
            buf.copy_from_slice(&page(slot)[..self.block_size]);
            return Ok(());
        }
        self.stats.misses += 1;
        let index = self.fill(io, block, 1)?;
        buf.copy_from_slice(&page(&self.slots[index])[..self.block_size]);
        Ok(())
    }

    /// Replace `block` with `buf` (one block long) and mark it dirty. `fresh`
    /// says the block was just allocated (see [`roles::Phase::Fresh`]).
    pub fn write(
        &mut self,
        io: &dyn BlockIo,
        block: u64,
        buf: &[u8],
        fresh: bool,
    ) -> Result<(), IoError> {
        let index = match self.map.get(&block) {
            Some(&index) => index,
            None => {
                if self.dirty >= self.dirty_limit {
                    self.flush(io)?; // bound the dirty set before it grows
                }
                let index = self.take_slot(io, true)?;
                self.map.insert(block, index);
                self.slots[index].block = Some(block);
                index
            }
        };
        let slot = &mut self.slots[index];
        page_mut(slot)[..self.block_size].copy_from_slice(buf);
        slot.referenced = true;
        slot.fresh |= fresh;
        if !slot.dirty {
            slot.dirty = true;
            self.dirty += 1;
        }
        Ok(())
    }

    /// Drop every clean block and give its page back (memory pressure).
    /// Dirty blocks stay: they are written back first, by a writeback.
    pub fn shrink(&mut self) -> usize {
        let mut released = 0;
        for index in 0..self.slots.len() {
            let slot = &mut self.slots[index];
            if slot.dirty || slot.page.is_none() {
                continue;
            }
            if let Some(block) = slot.block.take() {
                self.map.remove(&block);
            }
            slot.page = None;
            self.pages -= 1;
            self.bare.push(index);
            released += 1;
        }
        self.free.clear();
        released
    }

    /// Read `block` and the `wanted - 1` blocks after it (fewer when one is
    /// already cached or memory is short), plus read-ahead when the read
    /// continues the last miss, in one request. Returns the slot holding
    /// `block`; the others are in the map.
    fn fill(&mut self, io: &dyn BlockIo, block: u64, wanted: usize) -> Result<usize, IoError> {
        let mut taken = alloc::vec![self.take_slot(io, true)?];
        let sequential = self
            .last_miss
            .is_some_and(|last| block > last && block - last <= self.readahead as u64);
        let ahead = if sequential { self.readahead } else { 0 };
        let end = (block + (wanted.max(1) + ahead) as u64).min(self.blocks_count);
        for next in block + 1..end {
            if taken.len() >= self.max_run || self.map.contains_key(&next) {
                break;
            }
            match self.take_slot(io, false) {
                Ok(index) => taken.push(index),
                Err(_) => break, // read-ahead never forces a writeback
            }
        }
        let result = {
            let size = self.block_size;
            let mut bufs = disjoint_pages(&mut self.slots, &taken, size);
            io.read_sectors_vectored(block * self.sectors_per_block, &mut bufs)
        };
        if let Err(error) = result {
            self.free.extend(taken);
            return Err(error);
        }
        for (offset, &index) in taken.iter().enumerate() {
            let at = block + offset as u64;
            let slot = &mut self.slots[index];
            slot.block = Some(at);
            slot.referenced = offset < wanted;
            self.map.insert(at, index);
        }
        self.stats.readahead += taken.len().saturating_sub(wanted.max(1)) as u64;
        self.last_miss = Some(block + taken.len() as u64 - 1);
        Ok(taken[0])
    }

    /// A slot with a page and no block: a free one, a new page, or an evicted
    /// clean block. With `may_flush`, a cache full of dirty blocks is written
    /// back to make room; without it that case fails.
    fn take_slot(&mut self, io: &dyn BlockIo, may_flush: bool) -> Result<usize, IoError> {
        if let Some(index) = self.free.pop() {
            return Ok(index);
        }
        if self.pages < self.limit {
            if let Some(page) = self.memory.alloc() {
                self.pages += 1;
                let slot = Slot {
                    block: None,
                    page: Some(page),
                    dirty: false,
                    fresh: false,
                    referenced: false,
                };
                return Ok(match self.bare.pop() {
                    Some(index) => {
                        self.slots[index] = slot;
                        index
                    }
                    None => {
                        self.slots.push(slot);
                        self.slots.len() - 1
                    }
                });
            }
        }
        if let Some(index) = self.evict() {
            return Ok(index);
        }
        if !may_flush || self.dirty == 0 {
            return Err(IoError::Failed); // no memory at all
        }
        self.flush(io)?;
        self.evict().ok_or(IoError::Failed)
    }

    /// CLOCK: the first clean, unreferenced block (clearing reference bits
    /// on the way) loses its slot. `None` when every block is dirty.
    fn evict(&mut self) -> Option<usize> {
        let count = self.slots.len();
        for _ in 0..2 * count {
            let index = self.hand % count;
            self.hand = (index + 1) % count;
            let slot = &mut self.slots[index];
            let Some(block) = slot.block else { continue };
            if slot.dirty || slot.page.is_none() {
                continue;
            }
            if slot.referenced {
                slot.referenced = false;
                continue;
            }
            slot.block = None;
            self.map.remove(&block);
            self.stats.evictions += 1;
            return Some(index);
        }
        None
    }
}

/// Mutable views of the first `size` bytes of each slot in `wanted` (distinct
/// indices), in `wanted`'s order: the buffers of one vectored read.
fn disjoint_pages<'a>(slots: &'a mut [Slot], wanted: &[usize], size: usize) -> Vec<&'a mut [u8]> {
    let mut order: Vec<(usize, usize)> = wanted
        .iter()
        .enumerate()
        .map(|(position, &index)| (index, position))
        .collect();
    order.sort_unstable();
    let mut by_position: Vec<Option<&'a mut [u8]>> = (0..wanted.len()).map(|_| None).collect();
    let mut pending = order.iter().peekable();
    for (index, slot) in slots.iter_mut().enumerate() {
        let Some(&&(want, position)) = pending.peek() else {
            break;
        };
        if want == index {
            by_position[position] = Some(&mut page_mut(slot)[..size]);
            pending.next();
        }
    }
    by_position.into_iter().flatten().collect()
}

fn page(slot: &Slot) -> &[u8] {
    slot.page.as_deref().map_or(&[], |page| page.bytes())
}

fn page_mut(slot: &mut Slot) -> &mut [u8] {
    slot.page
        .as_deref_mut()
        .map_or(&mut [], |page| page.bytes_mut())
}
