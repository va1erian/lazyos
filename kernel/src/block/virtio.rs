//! Minimal virtio-blk driver over the legacy PCI interface (issue #100).
//!
//! QEMU's `-drive if=virtio` creates a transitional `1af4:1001` device whose
//! BAR0 is the virtio 0.9.5 I/O window: this driver drives that interface. It
//! negotiates no feature bits, sets up queue 0 as a split virtqueue in a
//! static, physically contiguous region, and completes one block request at a
//! time (registry callers are serialised by the driver's mutex).
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

use super::{pci, BlockDevice, BlockError, SECTOR_SIZE};
use crate::arch::io::{inb, inl, inw, outb, outl, outw};
use core::cell::UnsafeCell;
use core::sync::atomic::{fence, Ordering};
use spin::Mutex;
use x86_64::VirtAddr;

/// Legacy virtqueue size: QEMU reports 256 descriptors for virtio-blk.
const MAX_QUEUE: usize = 256;
/// The legacy spec requires the used ring 4096-aligned; 8192 = align(4096+518).
const MAX_USED_OFF: usize = 8192;
const MAX_USED_BYTES: usize = 6 + MAX_QUEUE * 8;
const QUEUE_BYTES: usize = MAX_USED_OFF + MAX_USED_BYTES;

/// One request moves at most this many sectors through the bounce page.
const REQUEST_SECTORS: usize = 8;
const REQUEST_BYTES: usize = REQUEST_SECTORS * SECTOR_SIZE;

// Descriptor flags (virtio 0.9.5).
const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
// Block request types.
const BLK_IN: u32 = 0;
const BLK_OUT: u32 = 1;

// Legacy virtio PCI register offsets from the I/O BAR.
const GUEST_FEATURES: u16 = 4;
const QUEUE_ADDRESS: u16 = 8;
const QUEUE_SIZE_REG: u16 = 12;
const QUEUE_SELECT: u16 = 14;
const QUEUE_NOTIFY: u16 = 16;
const DEVICE_STATUS: u16 = 18;
const ISR: u16 = 19;
const DEVICE_CONFIG: u16 = 20;

// Device status bits.
const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

/// The split virtqueue and rings. `static` memory is one contiguous region of
/// the loaded kernel image, which is what the legacy queue-address register
/// (a physical page frame number) requires.
#[repr(C, align(4096))]
struct Queue(UnsafeCell<[u8; QUEUE_BYTES]>);

// Safety: every access goes through `VirtioBlk::state`, whose mutex is held
// from the first descriptor write until the completion status is read.
unsafe impl Sync for Queue {}

static QUEUE: Queue = Queue(UnsafeCell::new([0; QUEUE_BYTES]));

/// The request header (16 bytes) plus the status byte. Kept on one page so
/// both DMA targets are physically contiguous.
#[repr(C)]
struct Control {
    header: [u8; 16],
    status: u8,
}

#[repr(C, align(64))]
struct ControlCell(UnsafeCell<Control>);

// Safety: like `Queue`, only touched under the driver's mutex.
unsafe impl Sync for ControlCell {}

static CONTROL: ControlCell = ControlCell(UnsafeCell::new(Control {
    header: [0; 16],
    status: 0,
}));

/// The bounce page for request payloads: page-aligned and exactly one page,
/// so its physical address covers the whole transfer.
#[repr(C, align(4096))]
struct Bounce(UnsafeCell<[u8; REQUEST_BYTES]>);

// Safety: like `Queue`, only touched under the driver's mutex.
unsafe impl Sync for Bounce {}

static BOUNCE: Bounce = Bounce(UnsafeCell::new([0; REQUEST_BYTES]));

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
    header_phys: u64,
    data_phys: u64,
    status_phys: u64,
}

/// The driver singleton. `None` until [`probe`] attaches.
pub struct VirtioBlk {
    state: Mutex<Option<State>>,
}

impl VirtioBlk {
    const fn new() -> VirtioBlk {
        VirtioBlk {
            state: Mutex::new(None),
        }
    }
}

static VIRTIO_BLK: VirtioBlk = VirtioBlk::new();

// The virtio legacy I/O window is plain memory-mapped-as-ports register
// space: every offset is documented (virtio 0.9.5 spec) as either a status
// register (safe to read repeatedly) or a control register this driver
// writes in the documented order, so the raw `arch::io` ops apply directly.

fn out8(port: u16, value: u8) {
    // Safety: see the module note above.
    unsafe { outb(port, value) };
}

fn out16(port: u16, value: u16) {
    // Safety: see the module note above.
    unsafe { outw(port, value) };
}

fn out32(port: u16, value: u32) {
    // Safety: see the module note above.
    unsafe { outl(port, value) };
}

fn in8(port: u16) -> u8 {
    // Safety: see the module note above.
    unsafe { inb(port) }
}

fn in16(port: u16) -> u16 {
    // Safety: see the module note above.
    unsafe { inw(port) }
}

fn in32(port: u16) -> u32 {
    // Safety: see the module note above.
    unsafe { inl(port) }
}

/// A 64-bit device register is two little-endian 32-bit halves.
fn in64(port: u16) -> u64 {
    let low = u64::from(in32(port));
    let high = u64::from(in32(port + 4));
    (high << 32) | low
}

/// Find a virtio-blk function, bring up its legacy queue, and return the
/// driver singleton for registration.
pub fn probe() -> Option<&'static dyn BlockDevice> {
    let device = pci::find_any(pci::VIRTIO_VENDOR, &[0x1001, 0x1042])?;
    let bar0 = pci::bar(device, 0);
    if bar0 & 1 == 0 {
        serial_println!(
            "virtio-blk: 1af4:{:04x} is modern-only (no legacy I/O BAR); \
             capability-based setup is not implemented",
            device.id
        );
        return None;
    }
    let io = (bar0 & !0x3) as u16;
    // Safety: `io` is the legacy window of a virtio function we just found.
    let state = unsafe { attach(io) }?;
    serial_println!(
        "virtio-blk: 1af4:{:04x} bus {}.{} io {:#x}",
        device.id,
        device.bus,
        device.device,
        io
    );
    *VIRTIO_BLK.state.lock() = Some(state);
    Some(&VIRTIO_BLK)
}

/// Reset the device, pick queue 0, point it at the static region, and go.
///
/// # Safety
/// `io` must be the legacy I/O window of a virtio device.
unsafe fn attach(io: u16) -> Option<State> {
    out8(io + DEVICE_STATUS, 0); // reset
    out8(io + DEVICE_STATUS, STATUS_ACK);
    out8(io + DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER);
    // Accept no feature bits: the base block commands are all the FAT reader
    // needs, and the legacy interface has no FEATURES_OK step.
    out32(io + GUEST_FEATURES, 0);
    out16(io + QUEUE_SELECT, 0);
    let qsize = in16(io + QUEUE_SIZE_REG);
    if qsize == 0 || usize::from(qsize) > MAX_QUEUE {
        return None;
    }
    let avail_off = usize::from(qsize) * 16;
    let used_off = (avail_off + 6 + usize::from(qsize) * 2 + 4095) & !4095;
    if used_off + 6 + usize::from(qsize) * 8 > QUEUE_BYTES {
        return None;
    }

    // Zero the rings before the device can write them, then hand over the
    // page frame number the legacy queue-address register wants.
    core::ptr::write_bytes(QUEUE.0.get() as *mut u8, 0, QUEUE_BYTES);
    let queue_phys = super::virt_to_phys(VirtAddr::from_ptr(QUEUE.0.get()))?.as_u64();
    if queue_phys >= 1 << 32 {
        return None; // the legacy register holds a 32-bit PFN
    }
    out32(io + QUEUE_ADDRESS, (queue_phys >> 12) as u32);

    // Legacy device config starts at offset 20: capacity in 512-byte sectors.
    let sectors = in64(io + DEVICE_CONFIG);
    if sectors == 0 {
        return None;
    }

    let header_phys =
        super::virt_to_phys(VirtAddr::from_ptr(CONTROL.0.get() as *const u8))?.as_u64();
    let data_phys = super::virt_to_phys(VirtAddr::from_ptr(BOUNCE.0.get() as *const u8))?.as_u64();

    out8(
        io + DEVICE_STATUS,
        STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK,
    );
    let _ = in8(io + ISR); // clear any stale interrupt

    Some(State {
        io,
        sectors,
        qsize,
        avail_off,
        used_off,
        avail_idx: 0,
        used_idx: 0,
        header_phys,
        data_phys,
        status_phys: header_phys + 16,
    })
}

/// Write descriptor `index`.
unsafe fn write_desc(queue: *mut u8, index: usize, addr: u64, len: u32, flags: u16, next: u16) {
    let desc = queue.add(index * 16);
    (desc as *mut u64).write_volatile(addr);
    (desc.add(8) as *mut u32).write_volatile(len);
    (desc.add(12) as *mut u16).write_volatile(flags);
    (desc.add(14) as *mut u16).write_volatile(next);
}

impl State {
    /// Submit one request for `bytes` (already copied into the bounce page for
    /// writes) and wait for the used ring to report it. The caller holds the
    /// driver lock, so exactly one request is outstanding.
    fn complete(&mut self, write: bool, lba: u64, bytes: usize) -> Result<(), BlockError> {
        if bytes == 0 || bytes > REQUEST_BYTES || bytes % SECTOR_SIZE != 0 {
            return Err(BlockError::Unsupported);
        }
        // Header: request type, reserved, starting sector.
        // Safety: the control block is exclusively ours while the lock is held.
        unsafe {
            let control = CONTROL.0.get();
            let header = (*control).header.as_mut_ptr();
            (header as *mut u32).write_volatile(if write { BLK_OUT } else { BLK_IN });
            (header.add(4) as *mut u32).write_volatile(0);
            (header.add(8) as *mut u64).write_volatile(lba);
            // A non-zero sentinel distinguishes "device answered" from "device
            // never touched it".
            (*control).status = 0xFF;
        }

        // Chain: header (device readable), data (writable for BLK_IN), status.
        // Safety: the queue static is exclusively ours while the lock is held.
        unsafe {
            let queue = QUEUE.0.get() as *mut u8;
            write_desc(queue, 0, self.header_phys, 16, DESC_NEXT, 1);
            let data_flags = DESC_NEXT | if write { 0 } else { DESC_WRITE };
            write_desc(queue, 1, self.data_phys, bytes as u32, data_flags, 2);
            write_desc(queue, 2, self.status_phys, 1, DESC_WRITE, 0);

            let avail = queue.add(self.avail_off);
            let slot = usize::from(self.avail_idx % self.qsize);
            (avail.add(4 + slot * 2) as *mut u16).write_volatile(0);
            self.avail_idx = self.avail_idx.wrapping_add(1);
            (avail.add(2) as *mut u16).write_volatile(self.avail_idx);
        }
        // Publish descriptors before ringing the doorbell.
        fence(Ordering::Release);
        out16(self.io + QUEUE_NOTIFY, 0);

        // Poll the used ring. The spin bound turns a wedged device into an
        // error instead of a hang.
        let mut spins = 0u32;
        loop {
            fence(Ordering::Acquire);
            // Safety: the used index is a device-written u16 in our queue.
            let seen = unsafe {
                ((QUEUE.0.get() as *const u8).add(self.used_off + 2) as *const u16).read_volatile()
            };
            if seen != self.used_idx {
                break;
            }
            spins += 1;
            if spins == 10_000_000 {
                return Err(BlockError::Io);
            }
            core::hint::spin_loop();
        }
        self.used_idx = self.used_idx.wrapping_add(1);
        // Safety: as above, the status byte is device-written.
        let status = unsafe { (*CONTROL.0.get()).status };
        let _ = in8(self.io + ISR); // deassert the legacy interrupt
        if status == 0 {
            Ok(())
        } else {
            Err(BlockError::Io)
        }
    }
}

impl BlockDevice for VirtioBlk {
    fn name(&self) -> &'static str {
        "virtio0"
    }

    fn sector_count(&self) -> u64 {
        self.state.lock().as_ref().map_or(0, |state| state.sectors)
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        let mut guard = self.state.lock();
        let state = guard.as_mut().ok_or(BlockError::Io)?;
        super::check_range(SECTOR_SIZE, state.sectors, lba, buf.len())?;
        let mut sector = 0u64;
        for chunk in buf.chunks_mut(REQUEST_BYTES) {
            state.complete(false, lba + sector, chunk.len())?;
            // Safety: the bounce page was filled by the completed request.
            let bounce =
                unsafe { core::slice::from_raw_parts(BOUNCE.0.get() as *const u8, chunk.len()) };
            chunk.copy_from_slice(bounce);
            sector += (chunk.len() / SECTOR_SIZE) as u64;
        }
        Ok(())
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        let mut guard = self.state.lock();
        let state = guard.as_mut().ok_or(BlockError::Io)?;
        super::check_range(SECTOR_SIZE, state.sectors, lba, buf.len())?;
        let mut sector = 0u64;
        for chunk in buf.chunks(REQUEST_BYTES) {
            // Safety: the bounce page is exclusively ours while the lock is held.
            let bounce =
                unsafe { core::slice::from_raw_parts_mut(BOUNCE.0.get() as *mut u8, chunk.len()) };
            bounce.copy_from_slice(chunk);
            state.complete(true, lba + sector, chunk.len())?;
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
