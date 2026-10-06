//! The modern (virtio 1.x) transport for the in-kernel virtio-blk (issue
//! #497, driver-plan D7): the same `libs/virtio` code the userspace drivers
//! use, over BAR windows the kernel maps for itself.
//!
//! The virtio capabilities in configuration space say which memory BAR holds
//! each structure; each BAR is mapped once, uncached, through
//! [`crate::mem::mmio::map_kernel`], only as far as the structures reach and
//! only after its base was checked against the device table's enumerated
//! resources. Everything the device reports afterwards goes through the
//! transport's own bounds checks.

use virtio::caps::{self, Location};
use virtio::transport::{Kick, Transport};

use super::io::{clear_queue, layout};
use super::queue::MAX_QUEUE;
use super::ring::Ring;
use super::Slot;
use crate::dev::pci::{self, Address};
use crate::dev::{BarKind, BusId};

const PAGE: u64 = 4096;

/// Read `width` bytes of `address`'s configuration space.
fn config(address: Address, offset: u8, width: u8) -> u32 {
    match width {
        1 => u32::from(pci::read8(address, offset)),
        2 => u32::from(pci::read16(address, offset)),
        _ => pci::read32(address, offset),
    }
}

/// The memory BAR `index` of the function at `address`, from the device
/// table (sized at enumeration): `(base, len)`.
fn memory_bar(address: Address, index: u8) -> Option<(u64, u64)> {
    let table = crate::dev::table().lock();
    let info = table.iter().find(|info| info.bus == BusId::Pci(address))?;
    let bar = info.resources.bar(index)?;
    (bar.kind == BarKind::Mem && bar.base != 0 && bar.base.is_multiple_of(PAGE))
        .then_some((bar.base, bar.len))
}

/// Build the modern transport of `function`, mapping the BAR windows its
/// capabilities point into. `Err` (with the reason) when the function has no
/// usable modern interface; the caller then falls back to legacy.
pub(super) fn transport(function: pci::Function) -> Result<Transport, &'static str> {
    let address = function.address;
    let caps = caps::parse(|offset, width| config(address, offset, width))
        .map_err(|_| "no virtio capabilities")?;
    let locations = [
        Some(caps.common),
        Some(caps.notify),
        Some(caps.isr),
        caps.device,
    ];
    // How far into each BAR the structures reach, in bytes: the bound checked
    // against the BAR. `map_kernel` maps whole pages, so a BAR shorter than a
    // page still gets one; only the structures inside it are ever touched.
    let mut reach = [0u64; 6];
    for location in locations.iter().flatten() {
        let bar = usize::from(location.bar);
        let end = u64::from(location.end().ok_or("structure wraps")?);
        let span = reach.get_mut(bar).ok_or("structure in a missing BAR")?;
        *span = (*span).max(end);
    }
    if caps.common.length < virtio::regs::common::LEN as u32 {
        return Err("common configuration too short");
    }
    // Every check, and every mapping (which can fail too), comes before the
    // function decodes memory or masters the bus, so a function refused here
    // (left to legacy or to nobody) gets neither from this path.
    let mut bases = [0u64; 6];
    for (index, &span) in reach.iter().enumerate().filter(|(_, &span)| span > 0) {
        let (base, len) =
            memory_bar(address, index as u8).ok_or("structure outside a memory BAR")?;
        if span > len {
            return Err("structure beyond its BAR");
        }
        bases[index] = crate::mem::mmio::map_kernel(base, span)?;
    }
    pci::enable_memory(address);
    pci::enable_bus_master(address);
    let at = |location: Location| {
        (bases[usize::from(location.bar)] + u64::from(location.offset)) as *mut u8
    };
    let (device, device_len) = match caps.device {
        Some(location) => (at(location), location.length),
        None => (core::ptr::null_mut(), 0),
    };
    // SAFETY: every structure lies inside its BAR (checked above), which is
    // mapped uncached from its base up to the furthest structure, and kernel
    // MMIO mappings are never removed.
    Ok(unsafe {
        Transport::new(
            at(caps.common),
            at(caps.notify),
            caps.notify.length,
            caps.notify_multiplier,
            at(caps.isr),
            device,
            device_len,
        )
    })
}

/// The largest power of two not above `value`.
fn floor_pow2(value: u16) -> u16 {
    if value == 0 {
        0
    } else {
        1 << value.ilog2()
    }
}

/// Reset the device (negotiation starts with one: it then touches no guest
/// memory), accept `VERSION_1` and nothing else, point queue 0 at the slot's
/// zeroed queue memory, and go. Returns the fresh ring and its doorbell.
///
/// # Safety
/// `transport` must be the device `slot` drives (or is about to), and nothing
/// else may use the slot's queue meanwhile.
pub(super) unsafe fn reset(slot: &Slot, transport: &Transport) -> Option<(Ring, Kick)> {
    // No feature but `VERSION_1`: like the legacy path, no write cache to
    // flush and no other request types.
    transport.negotiate(0, 0).ok()?;
    let qsize = floor_pow2(transport.queue_max(0).ok()?.min(MAX_QUEUE as u16));
    let (avail_off, used_off) = layout(qsize)?;
    // SAFETY: negotiation reset the device; the contract gives us the queue.
    let desc = unsafe { clear_queue(slot, avail_off) }?;
    let kick = transport
        .setup_queue_at(
            0,
            qsize,
            desc,
            desc + avail_off as u64,
            desc + used_off as u64,
        )
        .ok()?;
    transport.driver_ok().ok()?;
    let _ = transport.isr_status(); // clear any stale interrupt
    Some((Ring::new(qsize, avail_off, used_off), kick))
}
