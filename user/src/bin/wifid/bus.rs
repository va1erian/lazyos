//! What `libs/rtw88` cannot make itself (its `pci` module): the mapped
//! register BAR ([`Bar`], `rtw88::pci::Mmio`), DMA memory ([`Dma`],
//! `rtw88::pci::DmaAlloc`) and a delay ([`delay_us`]).

use rtw88::pci::{DmaAlloc, DmaBuf, Mmio};
use user::{dev, sys};

/// The chip's register BAR, mapped uncached by the kernel. A copy is a
/// second handle on the same registers (the failure dump keeps one while
/// the chip library owns the other).
#[derive(Clone)]
pub(super) struct Bar {
    base: *mut u8,
    len: u64,
}

impl Bar {
    /// Wrap a mapped BAR.
    ///
    /// # Safety
    /// `base` must be the start of a live uncached mapping of the card's
    /// register BAR, `len` bytes long, that outlives this value (the claim
    /// holds it until the task ends).
    pub(super) unsafe fn new(base: *mut u8, len: u64) -> Bar {
        Bar { base, len }
    }

    /// The register at `offset`, `T` wide.
    fn at<T>(&self, offset: u32) -> *mut T {
        let width = core::mem::size_of::<T>() as u64;
        // A register access outside the BAR or misaligned would be a stray
        // bus cycle: stop the driver instead (every offset is a named
        // constant of upstream's `reg.h`, so this guards future mistakes).
        assert!(
            u64::from(offset) + width <= self.len && u64::from(offset) % width == 0,
            "register {offset:#x} outside the BAR or misaligned"
        );
        // SAFETY: inside the mapping (checked above) and naturally aligned.
        unsafe { self.base.add(offset as usize).cast::<T>() }
    }
}

impl Mmio for Bar {
    fn read8(&self, offset: u32) -> u8 {
        // SAFETY: `at` returns an aligned pointer inside the live mapping; a
        // volatile access of exactly the register's width, as the chip needs.
        unsafe { self.at::<u8>(offset).read_volatile() }
    }

    fn read16(&self, offset: u32) -> u16 {
        // SAFETY: as in `read8`.
        unsafe { self.at::<u16>(offset).read_volatile() }
    }

    fn read32(&self, offset: u32) -> u32 {
        // SAFETY: as in `read8`.
        unsafe { self.at::<u32>(offset).read_volatile() }
    }

    fn write8(&self, offset: u32, val: u8) {
        // SAFETY: as in `read8`; device memory, so writing through a shared
        // reference is the hardware's semantics, not Rust aliasing.
        unsafe { self.at::<u8>(offset).write_volatile(val) }
    }

    fn write16(&self, offset: u32, val: u16) {
        // SAFETY: as in `write8`.
        unsafe { self.at::<u16>(offset).write_volatile(val) }
    }

    fn write32(&self, offset: u32, val: u32) {
        // SAFETY: as in `write8`.
        unsafe { self.at::<u32>(offset).write_volatile(val) }
    }

    fn delay_us(&self, us: u32) {
        delay_us(us);
    }
}

/// DMA blocks for the claimed card. Never freed while the driver runs: the
/// kernel treats freeing one as stopping the device (docs/architecture/
/// audio.md), and the claim's end returns them all.
pub(super) struct Dma {
    device: u64,
    blocks: usize,
}

/// Blocks the kernel allows one claim.
const MAX_BLOCKS: usize = 16;
/// Largest block the kernel allocates.
const MAX_BLOCK_BYTES: usize = 4 << 20;
const PAGE: usize = 4096;

impl Dma {
    pub(super) fn new(device: u64) -> Dma {
        Dma { device, blocks: 0 }
    }
}

// SAFETY: each block is a fresh `dma_alloc` run (physically contiguous
// whole frames from the kernel's DMA pool, so page aligned), mapped once by
// `buffer_map` for this task alone, never freed or unmapped while the claim
// lives (the claim's end returns them), and refused above 4 GiB below.
unsafe impl DmaAlloc for Dma {
    /// Zeroed, page-rounded, physically contiguous and below 4 GiB (no
    /// `ADDR64` flag: the 8822C's buffer descriptors hold 32-bit addresses).
    fn alloc(&mut self, len: usize) -> rtw88::error::Result<DmaBuf> {
        let len = len.div_ceil(PAGE) * PAGE;
        if len == 0 || len > MAX_BLOCK_BYTES || self.blocks == MAX_BLOCKS {
            return Err(rtw88::Error::NoMemory);
        }
        let (handle, phys) =
            dev::dma_alloc(self.device, len as u64, 0).map_err(|_| rtw88::Error::NoMemory)?;
        if phys.checked_add(len as u64).is_none_or(|end| end > 1 << 32) {
            return Err(rtw88::Error::Bus("DMA block above 4 GiB"));
        }
        let (virt, _) = sys::buffer_map(handle).map_err(|_| rtw88::Error::NoMemory)?;
        self.blocks += 1;
        Ok(DmaBuf {
            virt: virt as *mut u8,
            phys,
            len,
        })
    }
}

/// Wait at least `us` microseconds (upstream `udelay`, `usleep_range`,
/// `msleep`): spin on the monotonic clock below a tick, sleep above one.
pub(super) fn delay_us(us: u32) {
    let ns = u64::from(us) * 1_000;
    if ns >= sys::TICK_NS {
        sys::sleep_ns(ns);
        return;
    }
    let until = sys::monotonic_ns() + ns;
    while sys::monotonic_ns() < until {
        core::hint::spin_loop();
    }
}
