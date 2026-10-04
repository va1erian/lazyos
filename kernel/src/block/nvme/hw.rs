//! The machine side of `libs/nvme`'s `Platform` seam: DMA pages in the
//! kernel image, BAR0 registers, physical memory and a bring-up clock.

use core::cell::UnsafeCell;

use nvme::Platform;
use x86_64::VirtAddr;

/// One 4 KiB page of DMA memory in the kernel image.
#[repr(C, align(4096))]
pub(super) struct Page(pub(super) UnsafeCell<[u8; 4096]>);

// SAFETY: a page is only touched by its controller's owner, under the
// device lock (or before the device is published, at attach).
unsafe impl Sync for Page {}

impl Page {
    pub(super) const fn new() -> Page {
        Page(UnsafeCell::new([0; 4096]))
    }

    pub(super) fn virt(&self) -> u64 {
        self.0.get() as u64
    }

    pub(super) fn phys(&self) -> Option<u64> {
        crate::block::virt_to_phys(VirtAddr::new(self.virt())).map(|phys| phys.as_u64())
    }
}

/// The machine side of `libs/nvme`'s seam for one controller.
pub(super) struct Hw {
    /// Kernel virtual address of BAR0.
    pub(super) regs: u64,
    /// Bytes of BAR0 mapped at `regs`.
    pub(super) mapped: u64,
}

impl Platform for Hw {
    fn read32(&self, offset: usize) -> u32 {
        if offset as u64 + 4 > self.mapped {
            return u32::MAX;
        }
        // SAFETY: `regs` maps `mapped` bytes of this controller's BAR0
        // uncached, and the offset was just bounds-checked.
        unsafe { ((self.regs + offset as u64) as *const u32).read_volatile() }
    }

    fn write32(&self, offset: usize, value: u32) {
        if offset as u64 + 4 > self.mapped {
            return;
        }
        // SAFETY: as in `read32`; the library writes only registers and
        // doorbells of this controller.
        unsafe { ((self.regs + offset as u64) as *mut u32).write_volatile(value) }
    }

    fn read_mem(&self, phys: u64, buf: &mut [u8]) {
        let src = crate::mem::phys_to_virt(x86_64::PhysAddr::new(phys)).as_ptr::<u8>();
        for (index, byte) in buf.iter_mut().enumerate() {
            // SAFETY: the library reads only the queue, Identify and PRP
            // pages this driver handed it, all RAM in the physical map; the
            // controller may write them concurrently, hence volatile.
            *byte = unsafe { src.add(index).read_volatile() };
        }
    }

    fn write_mem(&self, phys: u64, data: &[u8]) {
        let dst = crate::mem::phys_to_virt(x86_64::PhysAddr::new(phys)).as_mut_ptr::<u8>();
        for (index, &byte) in data.iter().enumerate() {
            // SAFETY: as in `read_mem`: one of this controller's own pages.
            unsafe { dst.add(index).write_volatile(byte) };
        }
    }

    fn now_ns(&self) -> u64 {
        // Bring-up runs with interrupts off, where the tick-anchored
        // monotonic clock stands still: read the TSC directly.
        let per_tick = crate::arch::clock::cycles_per_tick();
        if per_tick == 0 {
            return crate::arch::clock::monotonic_ns();
        }
        (u128::from(crate::perf::rdtsc()) * 10_000_000 / u128::from(per_tick)) as u64
    }

    fn relax(&self) {
        core::hint::spin_loop();
    }
}
