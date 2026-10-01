//! Minimal virtio-blk driver over the legacy PCI interface (issue #100).
//!
//! QEMU's `-drive if=virtio` creates a transitional `1af4:1001` device whose
//! BAR0 is the virtio 0.9.5 I/O window: this driver drives that interface. It
//! negotiates no feature bits, sets up queue 0 as a split virtqueue in a
//! static, physically contiguous region, and completes one block request at a
//! time (registry callers are serialised by the driver's mutex).
//!
//! Each attached function owns one [`Slot`]: its own queue, request header and
//! bounce page, plus the registry-facing device. The device core offers
//! functions one at a time ([`attach_function`]), so a boot disk and a data
//! disk on separate virtio-blk functions are two independent block devices
//! (`virtio0`, `virtio1`, ...) that never share queue state.
//!
//! Modern-only devices (`1af4:1042`) are detected but not driven yet: they
//! expose their control structures through PCI capabilities and a memory BAR,
//! which needs BAR mapping in the kernel page table. That is the documented
//! next step; the default boot disk is ATA, so the legacy path covers QEMU
//! today and the ATA path keeps working untouched.
//!
//! DMA addresses come from [`super::virt_to_phys`] because the kernel heap maps
//! scattered physical frames. The descriptor table and rings live in a
//! `'static`; payloads bounce through a `'static` 4 KiB page, so the caller's
//! buffer may be a stack slice or a heap buffer without any physical-layout
//! requirement.

mod io;
mod queue;

use super::virtio_diag as diag;
use super::{BlockDevice, BlockError, SECTOR_SIZE};
use core::cell::UnsafeCell;
use core::sync::atomic::{fence, Ordering};
pub use io::attach_function;
use io::{in8, out16, ISR, QUEUE_NOTIFY};
use queue::{write_desc, Control, ControlCell, Queue, QUEUE_BYTES};
use spin::Mutex;

/// A request must complete within this many 100 Hz ticks (10 s).
const TIMEOUT_TICKS: u64 = 1000;
/// Spin bound used when the tick counter is not advancing (interrupts masked).
const SPIN_BACKSTOP: u64 = 4_000_000_000;

/// DMA granule: one descriptor per page, each page translated on its own
/// because neither statics nor the heap are promised to be physically contiguous.
const PAGE: usize = 4096;
/// One request moves at most this many bytes through the bounce region.
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const BOUNCE_PAGES: usize = MAX_REQUEST_BYTES / PAGE;
/// Header + one descriptor per page + status.
const MAX_CHAIN: usize = BOUNCE_PAGES + 2;

// Descriptor flags (virtio 0.9.5).
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
// Block request types.
const BLK_IN: u32 = 0;
const BLK_OUT: u32 = 1;

/// How many virtio-blk functions can be driven at once (one [`Slot`] each).
const MAX_VIRTIO: usize = 4;
/// Registry names, indexed by slot.
const NAMES: [&str; MAX_VIRTIO] = ["virtio0", "virtio1", "virtio2", "virtio3"];

/// The bounce region for request payloads: [`BOUNCE_PAGES`] page-aligned pages.
#[repr(C, align(4096))]
struct Bounce(UnsafeCell<[u8; MAX_REQUEST_BYTES]>);

// Safety: like `Queue`, only touched under the driver's mutex.
unsafe impl Sync for Bounce {}

/// Everything discovered at probe time. `avail_idx`/`used_idx` track the ring
/// positions; one outstanding request means they advance in lockstep.
#[derive(Clone, Copy)]
struct State {
    io: u16,
    sectors: u64,
    qsize: u16,
    avail_off: usize,
    used_off: usize,
    avail_idx: u16,
    used_idx: u16,
    /// A request was submitted and its completion not yet consumed (it timed out).
    outstanding: bool,
    header_phys: u64,
    /// Physical address of each bounce page, translated once at attach.
    page_phys: [u64; BOUNCE_PAGES],
    status_phys: u64,
}

/// One driven function: its DMA memory and the registry-facing device. The
/// device's state is `None` until [`attach_function`] claims the slot.
#[repr(C)]
struct Slot {
    queue: Queue,
    bounce: Bounce,
    control: ControlCell,
    device: VirtioBlk,
}

/// The registry-facing device of one [`Slot`].
pub struct VirtioBlk {
    /// Index into [`SLOTS`]; names the device and finds its DMA memory.
    index: usize,
    state: Mutex<Option<State>>,
}

impl Slot {
    const fn new(index: usize) -> Slot {
        Slot {
            queue: Queue(UnsafeCell::new([0; QUEUE_BYTES])),
            bounce: Bounce(UnsafeCell::new([0; MAX_REQUEST_BYTES])),
            control: ControlCell(UnsafeCell::new(Control {
                header: [0; 16],
                status: 0,
            })),
            device: VirtioBlk {
                index,
                state: Mutex::new(None),
            },
        }
    }
}

static SLOTS: [Slot; MAX_VIRTIO] = [Slot::new(0), Slot::new(1), Slot::new(2), Slot::new(3)];

impl State {
    /// Poll the used ring until the outstanding request completes; returns the
    /// spin count, or `None` on timeout. The wall-clock deadline is what bounds
    /// a wedged device: a spin count is not a duration, and a host stalled by
    /// load or by writing a fresh sparse image easily out-waits any fixed one.
    /// `ticks()` cannot advance with interrupts masked, so a very large spin
    /// count is the backstop for that case.
    fn wait_used(&self, slot: &Slot) -> Option<u64> {
        let start = crate::task::ticks();
        let mut spins = 0u64;
        loop {
            fence(Ordering::Acquire);
            // Safety: the used index is a device-written u16 in our queue.
            let seen = unsafe {
                ((slot.queue.0.get() as *const u8).add(self.used_off + 2) as *const u16)
                    .read_volatile()
            };
            if seen != self.used_idx {
                return Some(spins);
            }
            spins += 1;
            if spins.is_multiple_of(4096)
                && (crate::task::ticks().wrapping_sub(start) >= TIMEOUT_TICKS
                    || spins >= SPIN_BACKSTOP)
            {
                return None;
            }
            core::hint::spin_loop();
        }
    }

    /// Finish a request that timed out earlier. Until the device reports it,
    /// it may still read the bounce page or write the status byte, so nothing
    /// may reuse them. Errors when the device is still silent.
    fn drain(&mut self, slot: &Slot) -> Result<(), BlockError> {
        if self.outstanding {
            self.wait_used(slot).ok_or(BlockError::Io)?;
            self.outstanding = false;
            self.used_idx = self.used_idx.wrapping_add(1); // the late completion
        }
        Ok(())
    }

    /// Submit one request for `bytes` (already copied into the bounce page for
    /// writes) and wait for the used ring to report it. The caller holds the
    /// driver lock, so exactly one request is outstanding.
    fn complete(
        &mut self,
        slot: &Slot,
        write: bool,
        lba: u64,
        bytes: usize,
    ) -> Result<(), BlockError> {
        if bytes == 0 || bytes > MAX_REQUEST_BYTES || !bytes.is_multiple_of(SECTOR_SIZE) {
            return Err(BlockError::Unsupported);
        }
        debug_assert!(!self.outstanding, "drain() must run before a new request");
        // Header: request type, reserved, starting sector.
        // Safety: the control block is exclusively ours while the lock is held.
        unsafe {
            let control = slot.control.0.get();
            let header = (*control).header.as_mut_ptr();
            (header as *mut u32).write_volatile(if write { BLK_OUT } else { BLK_IN });
            (header.add(4) as *mut u32).write_volatile(0);
            (header.add(8) as *mut u64).write_volatile(lba);
            // A non-zero sentinel distinguishes "device answered" from "device
            // never touched it".
            (*control).status = 0xFF;
        }

        // Chain: header (device readable), one descriptor per bounce page
        // (writable for BLK_IN), status.
        // Safety: the queue static is exclusively ours while the lock is held,
        // and `attach` checked the queue holds `MAX_CHAIN` descriptors.
        unsafe {
            let queue = slot.queue.0.get() as *mut u8;
            let data_flags = if write { 0 } else { DESC_WRITE };
            write_desc(queue, 0, self.header_phys, 16, DESC_NEXT, 1);
            let pages = bytes.div_ceil(PAGE);
            for (page, phys) in self.page_phys[..pages].iter().enumerate() {
                let len = (bytes - page * PAGE).min(PAGE);
                let next = page as u16 + 2;
                write_desc(
                    queue,
                    page + 1,
                    *phys,
                    len as u32,
                    DESC_NEXT | data_flags,
                    next,
                );
            }
            write_desc(queue, pages + 1, self.status_phys, 1, DESC_WRITE, 0);

            let avail = queue.add(self.avail_off);
            let slot = usize::from(self.avail_idx % self.qsize);
            (avail.add(4 + slot * 2) as *mut u16).write_volatile(0);
            self.avail_idx = self.avail_idx.wrapping_add(1);
            (avail.add(2) as *mut u16).write_volatile(self.avail_idx);
        }
        // Publish descriptors before ringing the doorbell.
        fence(Ordering::Release);
        out16(self.io + QUEUE_NOTIFY, 0);

        self.outstanding = true;
        let spins = match self.wait_used(slot) {
            Some(spins) => spins,
            None => {
                // The device still owns the descriptors, the bounce page and the
                // status byte. `outstanding` stays set so the next request drains
                // this one first instead of mistaking its late completion for its own.
                diag::log(self.io, write, lba, bytes, "timeout", 0);
                return Err(BlockError::Io);
            }
        };
        // Consume the completion before anything else, so the next request
        // (including the next chunk of this transfer) waits for its own.
        self.outstanding = false;
        self.used_idx = self.used_idx.wrapping_add(1);
        if spins >= diag::SLOW_SPINS {
            diag::log(self.io, write, lba, bytes, "slow (completed)", spins);
        }
        // Safety: as above, the status byte is device-written.
        let status = unsafe { (*slot.control.0.get()).status };
        let _ = in8(self.io + ISR); // deassert the legacy interrupt
        if status == 0 {
            Ok(())
        } else {
            diag::log(self.io, write, lba, bytes, "status", u64::from(status));
            Err(BlockError::Io)
        }
    }
}

impl BlockDevice for VirtioBlk {
    fn name(&self) -> &'static str {
        NAMES[self.index]
    }

    fn sector_count(&self) -> u64 {
        self.state.lock().as_ref().map_or(0, |state| state.sectors)
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let slot = &SLOTS[self.index];
        let mut guard = self.state.lock();
        let state = guard.as_mut().ok_or(BlockError::Io)?;
        super::check_range(SECTOR_SIZE, state.sectors, lba, buf.len())?;
        state.drain(slot)?;
        let mut sector = 0u64;
        for chunk in buf.chunks_mut(MAX_REQUEST_BYTES) {
            state.complete(slot, false, lba + sector, chunk.len())?;
            // Safety: the bounce page was filled by the completed request.
            let bounce = unsafe {
                core::slice::from_raw_parts(slot.bounce.0.get() as *const u8, chunk.len())
            };
            chunk.copy_from_slice(bounce);
            sector += (chunk.len() / SECTOR_SIZE) as u64;
        }
        Ok(())
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        let slot = &SLOTS[self.index];
        let mut guard = self.state.lock();
        let state = guard.as_mut().ok_or(BlockError::Io)?;
        super::check_range(SECTOR_SIZE, state.sectors, lba, buf.len())?;
        state.drain(slot)?;
        let mut sector = 0u64;
        for chunk in buf.chunks(MAX_REQUEST_BYTES) {
            // Safety: the bounce page is exclusively ours while the lock is held.
            let bounce = unsafe {
                core::slice::from_raw_parts_mut(slot.bounce.0.get() as *mut u8, chunk.len())
            };
            bounce.copy_from_slice(chunk);
            state.complete(slot, true, lba + sector, chunk.len())?;
            sector += (chunk.len() / SECTOR_SIZE) as u64;
        }
        Ok(())
    }

    fn is_writable(&self) -> bool {
        self.state.lock().is_some()
    }

    fn flush(&self) -> Result<(), BlockError> {
        // No feature bits are negotiated, so the device advertises no write
        // cache to flush; a completed request is already ordered by QEMU.
        Ok(())
    }
}
