//! A physically contiguous DMA region a driver can both program into a device
//! (by bus address) and fill from ring 3 (by mapped address).

use user::dev;
use user::sys;

use super::error::Error;

/// Bytes per page: every DMA allocation is page-granular.
const PAGE: usize = 4096;

/// A region lives until the driver exits: the kernel treats a driver freeing
/// one of its DMA buffers as an explicit stop of the device (bus mastering is
/// cleared before the frames can be reused), so a running driver allocates
/// once and reuses, never frees.
pub(super) struct Region {
    va: *mut u8,
    bus: u64,
    len: usize,
}

impl Region {
    /// Allocate at least `len` zeroed bytes through the claimed device
    /// `device` and map them into this task.
    pub(super) fn alloc(device: u64, len: usize) -> Result<Region, Error> {
        let len = len.div_ceil(PAGE) * PAGE;
        let (handle, bus) = dev::dma_alloc(device, len as u64, 0).map_err(Error::Dev)?;
        let va = sys::buffer_map(handle)
            .map(|(va, _)| va)
            .map_err(Error::Dev)?;
        Ok(Region {
            va: va as *mut u8,
            bus,
            len,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn bus(&self, offset: usize) -> u64 {
        self.bus + offset as u64
    }

    /// Raw pointer at `offset`, for structures the device shares (queues).
    pub(super) fn ptr(&self, offset: usize) -> Result<*mut u8, Error> {
        if offset > self.len {
            return Err(Error::Range);
        }
        // SAFETY: `offset <= len`, inside (or one past) the mapping.
        Ok(unsafe { self.va.add(offset) })
    }

    /// A byte range as a mutable slice.
    ///
    /// The device may write the same bytes (a reply buffer), so callers copy
    /// out what they need and treat it as untrusted; they never hold the slice
    /// across a point where the device is allowed to write.
    pub(super) fn bytes(&mut self, offset: usize, len: usize) -> Result<&mut [u8], Error> {
        let end = offset.checked_add(len).ok_or(Error::Range)?;
        if end > self.len {
            return Err(Error::Range);
        }
        // SAFETY: `offset..end` is inside the mapping, which this task owns
        // for the life of the region.
        Ok(unsafe { core::slice::from_raw_parts_mut(self.va.add(offset), len) })
    }
}

/// Cache line size for [`flush`]: 64 bytes on every x86-64 part.
const LINE: usize = 64;

/// Write back and evict the cache lines of `len` bytes at `ptr`, so a device
/// that reads RAM without snooping the CPU caches sees what was just written.
///
/// The Intel HDA controller on the Kaby Lake box has `DEVC.NSNPEN` (PCI config
/// `0x78`, bit 11) set by the firmware, and the kernel lets no driver clear it
/// (`cfg_write` takes only the command register): its stream DMA then reads
/// the cached write-back buffer's stale RAM, and the codec is fed silence.
/// Flushing after each write is what Linux does when it runs a controller in
/// no-snoop mode. Harmless where the controller does snoop.
pub(super) fn flush(ptr: *const u8, len: usize) {
    let start = (ptr as usize) & !(LINE - 1);
    let end = (ptr as usize).saturating_add(len);
    let mut line = start;
    while line < end {
        // SAFETY: `clflush` of an address inside a mapping this task owns
        // (the callers pass pointers into a `Region`); it writes back and
        // evicts one line and changes no program-visible memory value.
        unsafe { core::arch::x86_64::_mm_clflush(line as *const u8) };
        line += LINE;
    }
    // SAFETY: a fence has no memory-safety requirement; `clflush` is only
    // ordered against stores by it.
    unsafe { core::arch::x86_64::_mm_mfence() };
}
