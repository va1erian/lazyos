//! Function attach (modern first, legacy for a legacy-only function), the
//! legacy virtio PCI port I/O window, and the queue memory both share.

use super::modern;
use super::queue::{MAX_QUEUE, QUEUE_BYTES};
use super::regs::Regs;
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
pub(super) const MAX_CHAIN: usize = super::plan::MAX_PIECES + 2;

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

/// Legacy device config starts at offset 20: capacity in 512-byte sectors,
/// two little-endian 32-bit halves.
pub(super) fn capacity(io: u16) -> u64 {
    let low = u64::from(in32(io + DEVICE_CONFIG));
    let high = u64::from(in32(io + DEVICE_CONFIG + 4));
    (high << 32) | low
}

/// The legacy device status register.
pub(super) fn status(io: u16) -> u8 {
    in8(io + DEVICE_STATUS)
}

/// Bring up one virtio-blk function in a free slot and return its device for
/// registration: through the modern transport whenever the function has the
/// virtio capabilities (a transitional or modern-only function), through the
/// legacy I/O window otherwise. `None` when it cannot be set up or every slot
/// is taken.
pub fn attach_function(function: pci::Function) -> Option<&'static dyn BlockDevice> {
    let address = function.address;
    let Some(slot) = SLOTS.iter().find(|slot| slot.device.state.lock().is_none()) else {
        serial_println!(
            "virtio-blk: no free slot for bus {}.{}",
            address.bus,
            address.device
        );
        return None;
    };
    let regs = match modern::transport(function) {
        Ok(transport) => Regs::Modern {
            transport,
            kick: None,
        },
        Err(why) => {
            let bar0 = pci::bar_raw(address, 0);
            if bar0 & 1 == 0 {
                serial_println!(
                    "virtio-blk: 1af4:{:04x} has neither a modern transport ({why}) \
                     nor a legacy I/O window",
                    function.id
                );
                return None;
            }
            Regs::Legacy {
                io: (bar0 & !0x3) as u16,
            }
        }
    };
    // Safety: `regs` reaches the function we were handed, and the slot is free.
    let state = unsafe { attach(slot, regs) }?;
    serial_println!(
        "virtio-blk: {} 1af4:{:04x} bus {}.{} {}, queue {}",
        NAMES[slot.device.index],
        function.id,
        address.bus,
        address.device,
        state.regs.describe(),
        state.ring.qsize
    );
    *slot.device.state.lock() = Some(state);
    Some(&slot.device)
}

/// Translate the control blocks, set the queue up and read the capacity.
///
/// # Safety
/// `regs` must reach a virtio-blk device, and `slot` must not already be
/// driving another one.
unsafe fn attach(slot: &Slot, mut regs: Regs) -> Option<State> {
    let mut control_phys = [0u64; MAX_INFLIGHT];
    for (phys, control) in control_phys.iter_mut().zip(&slot.controls) {
        let virt = VirtAddr::from_ptr(control.0.get() as *const u8);
        *phys = crate::block::virt_to_phys(virt)?.as_u64();
    }
    // SAFETY: forwarded from this function's contract.
    let ring = unsafe { regs.reset(slot) }?;
    let sectors = regs.capacity();
    if sectors == 0 {
        return None;
    }
    Some(State {
        regs,
        sectors,
        ring,
        control_phys,
    })
}

/// Where the available and used rings of a `qsize`-entry queue sit in the
/// slot's queue memory: the legacy layout (used ring on its own page), which
/// the modern transport accepts too. `None` when the size is unusable.
pub(super) fn layout(qsize: u16) -> Option<(usize, usize)> {
    if usize::from(qsize) < MAX_CHAIN || usize::from(qsize) > MAX_QUEUE {
        return None;
    }
    let avail_off = usize::from(qsize) * 16;
    let used_off = (avail_off + 6 + usize::from(qsize) * 2 + 4095) & !4095;
    (used_off + 6 + usize::from(qsize) * 8 <= QUEUE_BYTES).then_some((avail_off, used_off))
}

/// Zero `slot`'s queue memory and ask for no interrupts; returns its
/// physical address.
///
/// # Safety
/// The device must be reset (it touches no guest memory) and nothing else may
/// use the slot's queue.
pub(super) unsafe fn clear_queue(slot: &Slot, avail_off: usize) -> Option<u64> {
    let base = slot.queue.0.get() as *mut u8;
    // SAFETY: the queue static is `QUEUE_BYTES` long and, per the contract,
    // nobody else uses it.
    unsafe {
        core::ptr::write_bytes(base, 0, QUEUE_BYTES);
        // The driver polls the used ring: ask for no interrupts, so the
        // shared INTx line is not raised for every completion.
        (base.add(avail_off) as *mut u16).write_volatile(AVAIL_NO_INTERRUPT);
    }
    crate::block::virt_to_phys(VirtAddr::from_ptr(base)).map(|phys| phys.as_u64())
}

/// Reset the legacy device, pick queue 0, point it at the slot's zeroed
/// queue, and go. Returns the fresh ring.
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
    let (avail_off, used_off) = layout(qsize)?;
    // SAFETY: the device was just reset; the contract gives us the queue.
    let queue_phys = unsafe { clear_queue(slot, avail_off) }?;
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
