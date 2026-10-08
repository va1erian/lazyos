//! One physically contiguous DMA region, for the virtqueues and every packet
//! slot. It lives until the driver exits: the kernel treats a driver freeing
//! one of its DMA buffers as an explicit stop of the device (bus mastering is
//! cleared before the frames can be reused), so a running driver allocates
//! once and never frees (`docs/architecture/audio.md`, "never free a DMA
//! buffer while the device runs").

use nicdrv::DmaBlock;
use user::dev;
use user::sys;

use super::error::Error;

const PAGE: usize = 4096;

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

    /// The region as the driver core sees it.
    ///
    /// # Safety
    /// The caller must hand the block to `Queues` and touch the region only
    /// through it afterwards.
    pub(super) unsafe fn block(&self) -> DmaBlock {
        // SAFETY: `va` is a live mapping of `len` bytes, page aligned, and
        // `bus` is the device's address for it (`dma_alloc`'s contract).
        unsafe { DmaBlock::new(self.va, self.bus, self.len) }
    }
}
