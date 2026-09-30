//! The DMA pool: a contiguous physical region reserved once at boot (issue
//! #241, driver-plan section 3.4 and risk 2).
//!
//! The general frame allocator hands out single frames from a free list and a
//! lazy cursor, so it cannot produce the physically contiguous runs a
//! bus-mastering device needs. Rather than search the general pool (which
//! fragments), `mem::init` carves a fixed range out of the physical memory map
//! and keeps it aside. Pool frames stay in the refcount side table so
//! `share_frame`/`free_frame` keep working for the shared-buffer object, but
//! they are marked [`RESERVED`](super::frames::RESERVED) while free, which the
//! general allocator skips; only [`DmaPool::alloc`] hands them out.
//!
//! A pool frame returns here when its last reference drops
//! ([`super::Frames::release`]). The bitmap is protected by the frame allocator
//! lock itself ([`super::FRAMES`]), so there is no second lock to order: the
//! documented order `REGISTRY -> FRAMES` covers every DMA path.

use super::{FRAME_SIZE, MAX_REGIONS};

/// Target pool size: 16 MiB.
pub const TARGET_POOL_BYTES: u64 = 16 << 20;
/// Hard cap on pool pages; the bitmap is sized for it.
pub const MAX_POOL_PAGES: usize = 4096;
/// `u64` words in the free bitmap (one bit per page, set = free).
const BITMAP_WORDS: usize = MAX_POOL_PAGES / 64;
/// Align the pool base down to this so common device alignments are available.
const POOL_ALIGN: u64 = 2 << 20;
/// Physical address just past the 32-bit boundary: devices without 64-bit
/// addressing can only reach below it.
const FOUR_GIB: u64 = 1 << 32;

/// Snapshot of the pool's free space, for tests and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DmaStats {
    /// Physical base of the pool (0 when there is none).
    pub base: u64,
    /// Total pool pages.
    pub total_pages: u64,
    /// Pages currently free.
    pub free_pages: u64,
    /// Longest contiguous free run, in pages.
    pub largest_run: u64,
}

/// A contiguous physical region reserved for DMA. `pages == 0` means no pool
/// (tiny RAM); every operation then refuses cleanly.
#[derive(Clone, Copy)]
pub(super) struct DmaPool {
    base: u64,
    pages: u32,
    /// One bit per page, `1` = free.
    free: [u64; BITMAP_WORDS],
}

impl DmaPool {
    pub(super) const fn empty() -> DmaPool {
        DmaPool {
            base: 0,
            pages: 0,
            free: [0; BITMAP_WORDS],
        }
    }

    /// Reserve `pages` pages starting at `base`, all free.
    pub(super) fn reserve(&mut self, base: u64, pages: u32) {
        self.base = base;
        self.pages = pages;
        self.free.fill(0);
        for index in 0..pages as usize {
            self.free[index / 64] |= 1 << (index % 64);
        }
    }

    /// Whether `phys` is one of the pool's frames.
    pub(super) fn contains(&self, phys: u64) -> bool {
        self.pages != 0 && phys >= self.base && phys < self.base + self.pages as u64 * FRAME_SIZE
    }

    fn is_free(&self, index: usize) -> bool {
        self.free[index / 64] & (1 << (index % 64)) != 0
    }

    fn set_free(&mut self, index: usize, free: bool) {
        let mask = 1u64 << (index % 64);
        if free {
            self.free[index / 64] |= mask;
        } else {
            self.free[index / 64] &= !mask;
        }
    }

    /// First-fit allocation of `pages` contiguous free pages whose physical
    /// start is `align_pages`-page aligned (`align_pages` a power of two).
    /// Marks the run used; the caller sets refcounts and zeroes it.
    pub(super) fn alloc(&mut self, pages: u64, align_pages: u64) -> Option<u64> {
        if pages == 0 || self.pages == 0 || pages > u64::from(self.pages) {
            return None;
        }
        if align_pages == 0 || !align_pages.is_power_of_two() {
            return None;
        }
        let align = align_pages as usize;
        let align_bytes = align_pages.checked_mul(FRAME_SIZE)?;
        // First pool page whose physical address meets the alignment.
        let first = self.base.checked_add(align_bytes - 1)? & !(align_bytes - 1);
        if first < self.base {
            return None;
        }
        let mut index = ((first - self.base) / FRAME_SIZE) as usize;
        let need = pages as usize;
        while index + need <= self.pages as usize {
            if (0..need).all(|offset| self.is_free(index + offset)) {
                for offset in 0..need {
                    self.set_free(index + offset, false);
                }
                return Some(self.base + index as u64 * FRAME_SIZE);
            }
            index = index.checked_add(align)?;
        }
        None
    }

    /// Return one pool page to the free bitmap.
    pub(super) fn free(&mut self, phys: u64) {
        if self.contains(phys) {
            let index = ((phys - self.base) / FRAME_SIZE) as usize;
            self.set_free(index, true);
        }
    }

    pub(super) fn stats(&self) -> DmaStats {
        let mut free_pages = 0u64;
        let mut largest_run = 0u64;
        let mut run = 0u64;
        for index in 0..self.pages as usize {
            if self.is_free(index) {
                free_pages += 1;
                run += 1;
                largest_run = largest_run.max(run);
            } else {
                run = 0;
            }
        }
        DmaStats {
            base: self.base,
            total_pages: u64::from(self.pages),
            free_pages,
            largest_run,
        }
    }
}

/// Choose a contiguous pool from the usable regions: `min(16 MiB, usable/8)`
/// pages, below 4 GiB (32-bit DMA-capable devices), above the low megabyte,
/// and clear of the refcount table. `None` when no region has room (tiny RAM).
pub(super) fn choose_pool(
    starts: &[u64; MAX_REGIONS],
    ends: &[u64; MAX_REGIONS],
    count: usize,
    usable_frames: usize,
    table_phys: u64,
    table_frames: usize,
) -> Option<(u64, u32)> {
    let bytes = (usable_frames as u64 * FRAME_SIZE / 8).min(TARGET_POOL_BYTES) & !(FRAME_SIZE - 1);
    let frames = (bytes / FRAME_SIZE) as usize;
    if frames == 0 || frames > MAX_POOL_PAGES {
        return None;
    }
    let table_end = table_phys + table_frames as u64 * FRAME_SIZE;
    for index in 0..count {
        let start = starts[index];
        let end = ends[index].min(FOUR_GIB);
        if end < start + bytes {
            continue;
        }
        let mut base = (end - bytes) & !(FRAME_SIZE - 1);
        // Prefer a large-aligned base so device alignment requests are cheap;
        // fall back to a page-aligned base if that would leave the region.
        if base >= start {
            base &= !(POOL_ALIGN - 1);
        }
        if base < start {
            base = (end - bytes) & !(FRAME_SIZE - 1);
        }
        if base < start || base + bytes > end {
            continue;
        }
        if base < table_end && base + bytes > table_phys {
            continue;
        }
        return Some((base, frames as u32));
    }
    None
}

/// Allocate a contiguous, zeroed `pages`-page run from the pool. The run's
/// start is `align_pages`-page aligned. `None` on fragmentation or exhaustion;
/// nothing is leaked.
pub fn dma_alloc(pages: u64, align_pages: u64) -> Option<x86_64::PhysAddr> {
    let mut guard = super::FRAMES.lock();
    let frames = guard.as_mut()?;
    frames
        .dma_alloc(pages, align_pages)
        .map(x86_64::PhysAddr::new)
}

/// Snapshot of the pool's free space; see [`DmaStats`].
pub fn dma_stats() -> DmaStats {
    match super::FRAMES.lock().as_ref() {
        Some(frames) => frames.pool.stats(),
        None => DmaStats::default(),
    }
}

/// Test-only ordering log (issue #241): the DMA teardown contract is that the
/// device's bus-master bit is cleared before any of its frames return to the
/// pool. `dev::syscall::quiesce` notes [`QUIESCE`]; the pool release path notes
/// [`DMA_FREE`]; a test reads the events and asserts no free precedes a
/// quiesce.
#[cfg(lazyos_tests)]
pub mod order {
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicU64, Ordering};

    /// A device was quiesced (bus mastering off).
    pub const QUIESCE: u64 = 1;
    /// A DMA frame returned to the pool.
    pub const DMA_FREE: u64 = 2;

    const CAP: usize = 64;
    static LOG: [AtomicU64; CAP] = [const { AtomicU64::new(0) }; CAP];
    static LEN: AtomicU64 = AtomicU64::new(0);

    /// Append one event, keeping only the most recent [`CAP`].
    pub fn note(event: u64) {
        let index = LEN.fetch_add(1, Ordering::Relaxed) as usize;
        if index < CAP {
            LOG[index].store(event, Ordering::Relaxed);
        }
    }

    /// Forget every recorded event.
    pub fn reset() {
        LEN.store(0, Ordering::Relaxed);
        for slot in &LOG {
            slot.store(0, Ordering::Relaxed);
        }
    }

    /// The events recorded since the last [`reset`], oldest first.
    pub fn events() -> Vec<u64> {
        let len = LEN.load(Ordering::Relaxed) as usize;
        let len = len.min(CAP);
        (0..len)
            .map(|index| LOG[index].load(Ordering::Relaxed))
            .collect()
    }
}
