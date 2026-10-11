//! The PCIe bus (upstream `pci.c`, `pci.h`): TX/RX buffer-descriptor rings
//! in DMA memory, the interrupt mask and status (`HIMR`/`HISR`), the PCIe
//! PHY parameters over DBI and MDIO, and [`crate::hci::Hci`] for all of it.
//!
//! Owner: the core/MAC work package (it may become a directory).
//!
//! The binary gives this module two things it cannot make itself: the mapped
//! BAR ([`Mmio`]) and DMA memory ([`DmaAlloc`]). Neither is a syscall here.

use crate::error::Result;

/// The chip's memory-mapped registers (BAR 2, 64 KiB).
pub trait Mmio {
    fn read8(&self, offset: u32) -> u8;
    fn read16(&self, offset: u32) -> u16;
    fn read32(&self, offset: u32) -> u32;
    fn write8(&self, offset: u32, val: u8);
    fn write16(&self, offset: u32, val: u16);
    fn write32(&self, offset: u32, val: u32);
}

/// A block of DMA memory: where the CPU sees it and where the device does.
/// The memory stays valid for the life of the driver (a driver never frees
/// DMA while its device runs, docs/architecture/audio.md).
#[derive(Clone, Copy, Debug)]
pub struct DmaBuf {
    pub virt: *mut u8,
    pub phys: u64,
    pub len: usize,
}

/// Where DMA memory comes from (`dev::dma_alloc` in the binary, the heap in
/// tests). Blocks are below 4 GiB: the 8822C's buffer descriptors hold
/// 32-bit addresses. The kernel allows 16 blocks per claim, 4 MiB each.
pub trait DmaAlloc {
    fn alloc(&mut self, len: usize) -> Result<DmaBuf>;
}
