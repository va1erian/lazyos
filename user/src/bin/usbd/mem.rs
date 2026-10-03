//! The controller's register window and DMA memory, as `libs/xhci` sees them.
//!
//! [`Bar`] implements [`xhci::regs::Mmio`] over BAR 0 with bounds-checked
//! volatile accesses. [`Region`] is physically contiguous DMA memory: the
//! controller reaches it by bus address, the driver by mapped address. Like
//! `sndd`'s, a region is never freed while the driver runs: the kernel treats
//! freeing a DMA buffer as stopping the device. A detached device's region
//! goes back to a per-slot pool instead (`Hc::give_region`), so hot-plug
//! churn reuses memory rather than growing it.

use core::sync::atomic::{fence, Ordering};

use user::dev;
use user::sys;
use xhci::regs::Mmio;
use xhci::ring::RawMem;

use super::Error;

/// Bytes per page: every DMA allocation is page-granular.
pub(super) const PAGE: usize = 4096;

/// BAR 0, mapped.
pub(super) struct Bar {
    base: *mut u8,
    len: usize,
}

impl Bar {
    /// # Safety
    ///
    /// `base` must be the kernel's mapping of a `len`-byte memory BAR that
    /// stays mapped for the life of the value (the claim lives until exit).
    pub(super) unsafe fn new(base: *mut u8, len: usize) -> Bar {
        Bar { base, len }
    }

    fn at(&self, offset: usize) -> *mut u32 {
        assert!(
            offset.is_multiple_of(4) && offset + 4 <= self.len,
            "register {offset:#x} outside BAR 0"
        );
        // SAFETY: the assert keeps the dword inside the mapping.
        unsafe { self.base.add(offset).cast() }
    }
}

impl Mmio for Bar {
    fn read32(&self, offset: usize) -> u32 {
        // SAFETY: `at` bounds-checked and aligned the address; registers are
        // read with volatile loads so the compiler keeps every access.
        unsafe { self.at(offset).read_volatile() }
    }

    fn write32(&mut self, offset: usize, value: u32) {
        // Everything written to DMA memory before a register write (a ring,
        // a context) must be visible to the controller first.
        fence(Ordering::SeqCst);
        // SAFETY: as in `read32`.
        unsafe { self.at(offset).write_volatile(value) }
    }
}

/// A DMA region mapped into this task.
pub(super) struct Region {
    va: *mut u8,
    bus: u64,
    len: usize,
}

impl Region {
    /// At least `len` zeroed bytes through claimed device `device`.
    pub(super) fn alloc(device: u64, len: usize) -> Result<Region, Error> {
        let len = len.div_ceil(PAGE) * PAGE;
        let (handle, bus) = dev::dma_alloc(device, len as u64, 0).map_err(Error::Dev)?;
        let va = sys::display_map_buffer(handle).map_err(Error::Dev)?;
        Ok(Region {
            va: va as *mut u8,
            bus,
            len,
        })
    }

    /// The bus address of `offset`.
    pub(super) fn bus(&self, offset: usize) -> u64 {
        assert!(offset <= self.len);
        self.bus + offset as u64
    }

    /// A ring of `trbs` TRBs at `offset` (64-byte aligned by the caller).
    pub(super) fn ring(&self, offset: usize, trbs: usize) -> RawMem {
        assert!(offset.is_multiple_of(64) && offset + trbs * 16 <= self.len);
        // SAFETY: the window is inside this region, which lives until exit,
        // and each caller hands each window to exactly one ring.
        unsafe { RawMem::new(self.va.add(offset), trbs, self.bus(offset)) }
    }

    /// A dword window, for contexts and tables the driver builds in place.
    ///
    /// The controller reads these only after a command or doorbell the
    /// driver issues later (and `Bar::write32` fences before it), and writes
    /// output contexts only while executing a command, so the driver never
    /// holds the slice across a point where both sides write.
    pub(super) fn dwords(&mut self, offset: usize, count: usize) -> &mut [u32] {
        assert!(offset.is_multiple_of(4) && offset + count * 4 <= self.len);
        // SAFETY: the window is inside this region and dword-aligned.
        unsafe { core::slice::from_raw_parts_mut(self.va.add(offset).cast(), count) }
    }

    /// Store a little-endian `u64` (a DCBAA or scratchpad array entry).
    pub(super) fn write_u64(&mut self, offset: usize, value: u64) {
        let words = self.dwords(offset, 2);
        words[0] = value as u32;
        words[1] = (value >> 32) as u32;
    }

    /// Clear the whole region before it serves another device. Only called
    /// once the controller has let go of it (after Disable Slot).
    pub(super) fn zero(&mut self) {
        self.clear(0, self.len);
    }

    /// Clear `len` bytes at `offset`: a buffer about to receive a transfer
    /// that may come back short, so nothing stale is read after it.
    pub(super) fn clear(&mut self, offset: usize, len: usize) {
        assert!(offset.checked_add(len).is_some_and(|end| end <= self.len));
        for at in offset..offset + len {
            // SAFETY: inside the region (checked above); volatile like
            // every DMA access.
            unsafe { self.va.add(at).write_volatile(0) };
        }
    }

    /// Copy `data` to `offset`, for the device to read (a bulk OUT stage).
    pub(super) fn write(&mut self, offset: usize, data: &[u8]) {
        assert!(offset
            .checked_add(data.len())
            .is_some_and(|end| end <= self.len));
        // SAFETY: inside the region (checked above); the device reads it only
        // after the doorbell the caller rings next, which fences.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), self.va.add(offset), data.len()) };
    }

    /// Copy `out.len()` bytes the device wrote at `offset`. The copy is what
    /// the driver parses: the device could rewrite the buffer at any time.
    pub(super) fn read(&self, offset: usize, out: &mut [u8]) {
        assert!(offset + out.len() <= self.len);
        for (index, byte) in out.iter_mut().enumerate() {
            // SAFETY: inside the region; volatile because the device writes it.
            *byte = unsafe { self.va.add(offset + index).read_volatile() };
        }
    }
}
