//! The page cache of one open file: the frames a file-backed `mmap` maps.
//!
//! A mapping of a regular file cannot name the file's bytes directly (they
//! live in the block cache, or in a ramfs `Vec`), so it names *frames*: one
//! page of the file copied into a frame of its own, shared by every mapping of
//! the file. The cache holds one reference to each frame and a mapping one
//! more (the page-table entry), exactly as for a memfd (`mem::memfd`).
//!
//! The cache belongs to the file's [`super::openfile`] inode, so it is shared
//! by every description of the file and lives as long as one of them or a
//! mapping does. It is coherent with the Linux ABI's own reads and writes:
//!
//! * a `write`/`pwrite`/`ftruncate` updates the cached pages it overlaps
//!   (write-through: the file is written first, then the cached frames), so a
//!   mapping sees it at once;
//! * a store through a `MAP_SHARED` mapping changes only the frame, so a page
//!   that a writable shared mapping may have changed is *dirty*: a `read`
//!   takes its bytes from the frame ([`PageCache::overlay`]) and
//!   `msync`/`munmap`/`fsync`/exit and the periodic flusher write it back.
//!
//! Dirty is set by the first store through a writable shared mapping (the
//! write-protection fault, `mem::filemap`), and a writeback clears it *after*
//! write-protecting every mapping of the page, so the next store faults and
//! marks it again. While a page is being written back it is *flushing*: reads
//! still take its bytes from the frame, and a write to the file that lands
//! meanwhile dirties it again, so the flush cannot leave the file stale.
//!
//! Nothing here does I/O or holds the lock across it: filling a page reads the
//! file with no lock held and uses a generation counter to notice a write that
//! raced with the read (the caller retries).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::PhysAddr;

use crate::mem::{free_frame, frame_refcount, phys_to_virt, share_frame};

/// Bytes in a page.
pub const PAGE: u64 = 4096;

/// One cached page.
struct Page {
    frame: u64,
    dirty: bool,
    /// A writeback of the page is in flight ([`PageCache::take_dirty`]).
    flushing: bool,
}

/// The cached pages of one file, by page index.
pub struct PageCache {
    pages: Mutex<BTreeMap<u64, Page>>,
    /// Bumped by every write and truncate; a fill that read the file before a
    /// bump must not insert what it read.
    generation: AtomicU64,
}

/// A page to write back: its index and a frame reference the caller owns.
pub struct DirtyPage {
    pub index: u64,
    pub frame: PhysAddr,
}

impl Default for PageCache {
    fn default() -> Self {
        PageCache::new()
    }
}

impl PageCache {
    pub const fn new() -> PageCache {
        PageCache {
            pages: Mutex::new(BTreeMap::new()),
            generation: AtomicU64::new(0),
        }
    }

    /// The write generation, to be read before the file is.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Number of cached pages (tests and diagnostics).
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn len(&self) -> usize {
        self.pages.lock().len()
    }

    /// Whether any page is cached.
    pub fn is_empty(&self) -> bool {
        self.pages.lock().is_empty()
    }

    /// Whether any page is dirty (the flusher's cheap test).
    pub fn has_dirty(&self) -> bool {
        self.pages.lock().values().any(|page| page.dirty)
    }

    /// Page `index`'s frame with one reference taken for the caller.
    pub fn lookup(&self, index: u64) -> Option<PhysAddr> {
        let pages = self.pages.lock();
        let frame = PhysAddr::new(pages.get(&index)?.frame);
        share_frame(frame).then_some(frame)
    }

    /// Insert `frame` (whose reference the cache takes over) as page `index`,
    /// unless a write happened since `generation` was read or the page
    /// appeared meanwhile. Returns the frame to map, with a reference for the
    /// caller: `Ok(frame)` is either `frame` itself or the page that won, in
    /// which case `frame` was freed; `Err(())` means the generation moved and
    /// `frame` is still the caller's to free.
    pub fn insert(&self, index: u64, frame: PhysAddr, generation: u64) -> Result<PhysAddr, ()> {
        let mut pages = self.pages.lock();
        if let Some(existing) = pages.get(&index) {
            let existing = PhysAddr::new(existing.frame);
            if share_frame(existing) {
                drop(pages);
                free_frame(frame);
                return Ok(existing);
            }
            return Err(());
        }
        if self.generation.load(Ordering::Acquire) != generation {
            return Err(());
        }
        // The cache keeps `frame`'s allocation reference; the mapping gets one.
        if !share_frame(frame) {
            return Err(());
        }
        pages.insert(
            index,
            Page {
                frame: frame.as_u64(),
                dirty: false,
                flushing: false,
            },
        );
        Ok(frame)
    }

    /// A page the file's own write changed: copy `data` (written at file
    /// `offset`) into every cached page it overlaps. The generation moves
    /// once the frames hold the bytes, so a fill that read the file before
    /// the write cannot insert its stale page.
    pub fn update(&self, offset: u64, data: &[u8]) {
        let mut pages = self.pages.lock();
        if !pages.is_empty() && !data.is_empty() {
            let end = offset + data.len() as u64;
            for (&index, page) in pages.range_mut(offset / PAGE..=(end - 1) / PAGE) {
                let page_start = index * PAGE;
                let from = offset.max(page_start);
                let to = end.min(page_start + PAGE);
                let src = &data[(from - offset) as usize..(to - offset) as usize];
                copy_to_frame(page.frame, (from - page_start) as usize, src);
                // A writeback may have read the frame before these bytes:
                // it must not call the page clean.
                if page.flushing {
                    page.dirty = true;
                }
            }
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Replace the bytes of `buf` (read from file offset `offset`) that lie
    /// in a dirty or flushing page with the frame's, which are newer than
    /// the file's.
    pub fn overlay(&self, offset: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }
        let pages = self.pages.lock();
        if pages.is_empty() {
            return;
        }
        let end = offset + buf.len() as u64;
        for (&index, page) in pages.range(offset / PAGE..=(end - 1) / PAGE) {
            if !page.dirty && !page.flushing {
                continue;
            }
            let page_start = index * PAGE;
            let from = offset.max(page_start);
            let to = end.min(page_start + PAGE);
            let dst = &mut buf[(from - offset) as usize..(to - offset) as usize];
            copy_from_frame(page.frame, (from - page_start) as usize, dst);
        }
    }

    /// Mark pages `first..=last` that are cached as changed through a
    /// writable shared mapping.
    pub fn mark_dirty(&self, first: u64, last: u64) {
        let mut pages = self.pages.lock();
        for (_, page) in pages.range_mut(first..=last) {
            page.dirty = true;
        }
    }

    /// The dirty pages in `first..=last`, now *flushing*, each with a frame
    /// reference the caller must give back with [`PageCache::finish`]. The
    /// caller write-protects the mappings of these pages before it writes
    /// them.
    pub fn take_dirty(&self, first: u64, last: u64) -> Vec<DirtyPage> {
        let mut pages = self.pages.lock();
        let mut out = Vec::new();
        for (&index, page) in pages.range_mut(first..=last) {
            if !page.dirty || page.flushing {
                continue;
            }
            let frame = PhysAddr::new(page.frame);
            if out.try_reserve(1).is_err() {
                break;
            }
            if share_frame(frame) {
                page.dirty = false;
                page.flushing = true;
                out.push(DirtyPage { index, frame });
            }
        }
        out
    }

    /// A page's writeback ended: drop the caller's reference. A page that
    /// was not written, or that was written to meanwhile, is dirty again.
    pub fn finish(&self, page: DirtyPage, written: bool) {
        {
            let mut pages = self.pages.lock();
            if let Some(entry) = pages.get_mut(&page.index) {
                if entry.frame == page.frame.as_u64() {
                    entry.flushing = false;
                    if !written {
                        entry.dirty = true;
                    }
                }
            }
        }
        free_frame(page.frame);
    }

    /// The clean, idle pages of the cache with a reference each (the caller
    /// frees them): what a change made behind the cache's back refreshes.
    pub fn clean_pages(&self) -> Vec<(u64, PhysAddr)> {
        let pages = self.pages.lock();
        let mut out = Vec::new();
        for (&index, page) in pages.iter() {
            if page.dirty || page.flushing || out.try_reserve(1).is_err() {
                continue;
            }
            let frame = PhysAddr::new(page.frame);
            if share_frame(frame) {
                out.push((index, frame));
            }
        }
        out
    }

    /// Give back the pages of `first..=last` that no mapping holds (the cache
    /// is the only reference) and that are clean. A dirty page stays until a
    /// writeback cleans it.
    pub fn trim(&self, first: u64, last: u64) {
        let mut freed = Vec::new();
        {
            let mut pages = self.pages.lock();
            let stale: Vec<u64> = pages
                .range(first..=last)
                .filter(|(_, page)| {
                    !page.dirty && !page.flushing && frame_refcount(PhysAddr::new(page.frame)) == 1
                })
                .map(|(&index, _)| index)
                .collect();
            for index in stale {
                if let Some(page) = pages.remove(&index) {
                    freed.push(page.frame);
                }
            }
        }
        for frame in freed {
            free_frame(PhysAddr::new(frame));
        }
    }

    /// The file shrank to `size`: drop the pages wholly past the new end and
    /// zero the rest of the last one, so a later growth reads zeros. A
    /// mapping that still holds a dropped frame keeps it until it unmaps
    /// (where Linux would raise `SIGBUS` on the next touch).
    pub fn truncate(&self, size: u64) {
        let keep = size.div_ceil(PAGE);
        let mut freed = Vec::new();
        {
            let mut pages = self.pages.lock();
            let tail = pages.split_off(&keep);
            freed.extend(tail.into_values().map(|page| page.frame));
            let within = (size % PAGE) as usize;
            if within != 0 {
                if let Some(page) = pages.get(&(size / PAGE)) {
                    let zeros = [0u8; PAGE as usize];
                    copy_to_frame(page.frame, within, &zeros[within..]);
                }
            }
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        for frame in freed {
            free_frame(PhysAddr::new(frame));
        }
    }
}

impl Drop for PageCache {
    /// The file is gone (last description and mapping): the cache's reference
    /// to each page goes with it.
    fn drop(&mut self) {
        for page in core::mem::take(self.pages.get_mut()).into_values() {
            free_frame(PhysAddr::new(page.frame));
        }
    }
}

/// Copy `src` into `frame` at `within`.
pub fn copy_to_frame(frame: u64, within: usize, src: &[u8]) {
    debug_assert!(within + src.len() <= PAGE as usize);
    // SAFETY: `frame` is a live frame the cache owns, reachable through the
    // physical-memory window; callers split at page boundaries so the range
    // stays inside the 4 KiB frame, and `src` is a kernel buffer that does
    // not overlap it.
    unsafe {
        core::ptr::copy_nonoverlapping(
            src.as_ptr(),
            phys_to_virt(PhysAddr::new(frame))
                .as_mut_ptr::<u8>()
                .add(within),
            src.len(),
        );
    }
}

/// Copy bytes of `frame` at `within` into `dst`.
pub fn copy_from_frame(frame: u64, within: usize, dst: &mut [u8]) {
    debug_assert!(within + dst.len() <= PAGE as usize);
    // SAFETY: as `copy_to_frame`: a live frame, a range inside it, and a
    // kernel buffer that does not overlap it.
    unsafe {
        core::ptr::copy_nonoverlapping(
            phys_to_virt(PhysAddr::new(frame))
                .as_ptr::<u8>()
                .add(within),
            dst.as_mut_ptr(),
            dst.len(),
        );
    }
}
