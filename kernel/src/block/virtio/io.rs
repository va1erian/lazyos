//! Legacy virtio PCI port I/O, function attach and device reset.

use super::queue::{MAX_QUEUE, QUEUE_BYTES};
use super::ring::{Ring, AVAIL_NO_INTERRUPT, MAX_INFLIGHT};
use super::{Slot, State, NAMES, SLOTS};
use crate::arch::io::{inb, inl, inw, outb, outl, outw};
use crate::block::BlockDevice;
use crate::dev::pci;
use x86_64::VirtAddr;

// Legacy virtio PCI register offsets from the I/O BAR.
const GUEST_FEATURES: u16 = 4;
const QUEUE_ADDRESS: u16 = 8;
const QUEUE_SIZE_REG: u16 = 12;
const QUEUE_SELECT: u16 = 14;
pub(super) const QUEUE_NOTIFY: u16 = 16;
const DEVICE_STATUS: u16 = 18;
const ISR: u16 = 19;
const DEVICE_CONFIG: u16 = 20;

// Device status bits.
const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

/// Descriptors the largest request takes: header, data pieces, status.
const MAX_CHAIN: usize = super::plan::MAX_PIECES + 2;

// The virtio legacy I/O window is plain memory-mapped-as-ports register
// space: every offset is documented (virtio 0.9.5 spec) as either a status
// register (safe to read repeatedly) or a control register this driver
// writes in the documented order, so the raw `arch::io` ops apply directly.

fn out8(port: u16, value: u8) {
    // Safety: see the module note above.
    unsafe { outb(port, value) };
}

pub(super) fn out16(port: u16, value: u16) {
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

/// Bring up one virtio-blk function in a free slot and return its device for
/// registration. `None` when the function is modern-only, cannot be set up, or
/// every slot is taken.
pub fn attach_function(function: pci::Function) -> Option<&'static dyn BlockDevice> {
    let address = function.address;
    let bar0 = pci::bar_raw(address, 0);
    if bar0 & 1 == 0 {
        serial_println!(
            "virtio-blk: 1af4:{:04x} is modern-only (no legacy I/O BAR); \
             capability-based setup is not implemented",
            function.id
        );
        return None;
    }
    let Some(slot) = SLOTS.iter().find(|slot| slot.device.state.lock().is_none()) else {
        serial_println!(
            "virtio-blk: no free slot for bus {}.{}",
            address.bus,
            address.device
        );
        return None;
    };
    let io = (bar0 & !0x3) as u16;
    // Safety: `io` is the legacy window of a virtio function we were handed.
    let state = unsafe { attach(slot, io) }?;
    serial_println!(
        "virtio-blk: {} 1af4:{:04x} bus {}.{} io {:#x}",
        NAMES[slot.device.index],
        function.id,
        address.bus,
        address.device,
        io
    );
    *slot.device.state.lock() = Some(state);
    Some(&slot.device)
}

/// Translate the control blocks, set the queue up and read the capacity.
///
/// # Safety
/// `io` must be the legacy I/O window of a virtio device, and `slot` must not
/// already be driving another one.
unsafe fn attach(slot: &Slot, io: u16) -> Option<State> {
    let mut control_phys = [0u64; MAX_INFLIGHT];
    for (phys, control) in control_phys.iter_mut().zip(&slot.controls) {
        let virt = VirtAddr::from_ptr(control.0.get() as *const u8);
        *phys = crate::block::virt_to_phys(virt)?.as_u64();
    }
    // SAFETY: forwarded from this function's contract.
    let ring = unsafe { reset(slot, io) }?;
    // Legacy device config starts at offset 20: capacity in 512-byte sectors.
    let sectors = in64(io + DEVICE_CONFIG);
    if sectors == 0 {
        return None;
    }
    Some(State {
        io,
        sectors,
        ring,
        control_phys,
    })
}

/// Reset the device (it then touches no guest memory), pick queue 0, point
/// it at the slot's zeroed queue, and go. Returns the fresh ring.
///
/// # Safety
/// `io` must be the legacy I/O window of the virtio device `slot` drives (or
/// is about to), and nothing else may use the slot's queue meanwhile.
pub(super) unsafe fn reset(slot: &Slot, io: u16) -> Option<Ring> {
    out8(io + DEVICE_STATUS, 0); // reset
    out8(io + DEVICE_STATUS, STATUS_ACK);
    out8(io + DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER);
    // Accept no feature bits: the base block commands are all the
    // filesystems need, and the legacy interface has no FEATURES_OK step.
    out32(io + GUEST_FEATURES, 0);
    out16(io + QUEUE_SELECT, 0);
    let qsize = in16(io + QUEUE_SIZE_REG);
    if usize::from(qsize) < MAX_CHAIN || usize::from(qsize) > MAX_QUEUE {
        return None;
    }
    let avail_off = usize::from(qsize) * 16;
    let used_off = (avail_off + 6 + usize::from(qsize) * 2 + 4095) & !4095;
    if used_off + 6 + usize::from(qsize) * 8 > QUEUE_BYTES {
        return None;
    }

    // Zero the rings before the device can write them, then hand over the
    // page frame number the legacy queue-address register wants.
    let base = slot.queue.0.get() as *mut u8;
    // SAFETY: the queue static is `QUEUE_BYTES` long and, per the contract,
    // nobody else uses it; the device was just reset.
    unsafe {
        core::ptr::write_bytes(base, 0, QUEUE_BYTES);
        // The driver polls the used ring: ask for no interrupts, so the
        // shared INTx line is not raised for every completion.
        (base.add(avail_off) as *mut u16).write_volatile(AVAIL_NO_INTERRUPT);
    }
    let queue_phys = crate::block::virt_to_phys(VirtAddr::from_ptr(base))?.as_u64();
    if queue_phys >= 1 << 32 {
        return None; // the legacy register holds a 32-bit PFN
    }
    out32(io + QUEUE_ADDRESS, (queue_phys >> 12) as u32);
    out8(
        io + DEVICE_STATUS,
        STATUS_ACK | STATUS_DRIVER | STATUS_DRIVER_OK,
    );
    let _ = in8(io + ISR); // clear any stale interrupt
    Some(Ring::new(qsize, avail_off, used_off))
}
