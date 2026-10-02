//! The kernel side of the ext2 block cache (`libs/ext2fs/src/cache`): pages
//! from the frame allocator, the size knob, and the writeback surface the
//! periodic flusher (`fs/flusher.rs`) calls.
//!
//! Cache pages are whole physical frames reached through the physical-memory
//! window, not heap memory: the kernel heap is small (~16 MiB) and shared by
//! everything, while a useful cache is several MiB. Each page costs one small
//! heap box for its handle.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, Ordering};

use ext2fs::{CacheConfig, CacheMemory, CachePage, CACHE_PAGE_SIZE};
use x86_64::PhysAddr;

use super::Ext2;
use crate::fs::vfs::FsError;
use crate::mem;

/// Cache size in KiB, fixed at build time (`LAZYOS_BLOCK_CACHE_KB`; `0` turns
/// the cache off). Unset, each volume takes 1/32 of RAM, between 1 and 32 MiB.
const SIZE_KB: Option<&str> = option_env!("LAZYOS_BLOCK_CACHE_KB");
const MIN_PAGES: usize = 256;
const MAX_PAGES: usize = 8192;

/// The cache stops taking frames while fewer than 1/16 of all frames are free,
/// leaving them to processes; it then recycles its own pages instead.
const RESERVE_DIVISOR: usize = 16;

/// The configuration every cached mount gets.
pub(super) fn config() -> CacheConfig {
    let total = mem::frame_stats().total;
    let pages = match SIZE_KB.and_then(|kb| kb.trim().parse::<usize>().ok()) {
        Some(kb) => kb * 1024 / CACHE_PAGE_SIZE,
        None => (total / 32).clamp(MIN_PAGES, MAX_PAGES),
    };
    let memory = FrameMemory {
        reserve: total / RESERVE_DIVISOR,
    };
    CacheConfig::with_memory(pages, Box::new(memory))
}

/// Cache pages from the frame allocator.
pub(crate) struct FrameMemory {
    /// Free frames to leave alone.
    pub(crate) reserve: usize,
}

impl CacheMemory for FrameMemory {
    fn alloc(&self) -> Option<Box<dyn CachePage>> {
        // A write that fills the cache runs long with interrupts off and may
        // never reach the device: keep the keyboard drained (`input::ps2`).
        crate::input::ps2::service();
        if mem::frame_stats().free < self.reserve {
            return None;
        }
        let frame = mem::alloc_frame()?;
        Some(Box::new(FramePage(frame)))
    }
}

/// One frame owned by the cache; dropping it frees the frame.
struct FramePage(PhysAddr);

impl CachePage for FramePage {
    fn bytes(&self) -> &[u8] {
        // SAFETY: the frame was allocated for this page alone (refcount one,
        // never mapped into any address space) and stays allocated until
        // `drop`; the physical-memory window maps it for the kernel's life.
        unsafe { core::slice::from_raw_parts(mem::phys_to_virt(self.0).as_ptr(), CACHE_PAGE_SIZE) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `bytes`; `&mut self` makes this the only view.
        unsafe {
            core::slice::from_raw_parts_mut(mem::phys_to_virt(self.0).as_mut_ptr(), CACHE_PAGE_SIZE)
        }
    }
}

impl Drop for FramePage {
    fn drop(&mut self) {
        mem::free_frame(self.0);
    }
}

impl Ext2 {
    /// [`Ext2::open_cached`] with a cache of exactly `pages` frames and no
    /// reserve, so a test can make it tiny.
    #[cfg(lazyos_tests)]
    pub fn open_with_pages(
        device: &'static dyn crate::block::BlockDevice,
        pages: usize,
    ) -> Result<Ext2, FsError> {
        let memory = FrameMemory { reserve: 0 };
        Ext2::mount(
            device,
            Some(CacheConfig::with_memory(pages, Box::new(memory))),
        )
    }

    /// Write back what the cache holds without marking the volume clean (the
    /// periodic flusher). With `pressure`, also give clean pages back. The
    /// first failure per volume is logged; the error stays recorded for the
    /// next `flush` to report (`libs/ext2fs/src/commit.rs`).
    pub fn writeback(&self, pressure: bool) -> Result<(), FsError> {
        static LOGGED: AtomicBool = AtomicBool::new(false);
        let _gate = self.gate.lock();
        let result = self.volume.writeback();
        if pressure {
            self.volume.shrink_cache();
        }
        if result.is_err() && !LOGGED.swap(true, Ordering::Relaxed) {
            serial_println!(
                "ext2: {}: writeback failed; the next sync reports it",
                self.device
            );
        }
        Ok(result?)
    }

    /// The cache's counters, for diagnostics and tests.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn cache_stats(&self) -> Option<ext2fs::CacheStats> {
        self.volume.cache_stats()
    }

    /// Dirty blocks waiting for a writeback.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn dirty_blocks(&self) -> usize {
        self.volume.dirty_blocks()
    }

    /// Whether `s_state` carried the error bit at mount.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn had_errors_at_mount(&self) -> bool {
        self.volume.had_errors_at_mount()
    }
}
