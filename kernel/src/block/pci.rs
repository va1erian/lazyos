//! Minimal PCI configuration-space access over the legacy 0xCF8/0xCFC ports
//! (issue #100).
//!
//! Only what the block layer needs: enumerate every bus/device/function, match
//! a vendor/device pair, and read a BAR. Legacy config mechanism 1 is
//! available on every x86 machine LazyOS boots on, so there is no MMCONFIG
//! support yet; interrupts and MSI are equally out of scope because the
//! drivers poll.

use crate::arch::io::{inl, outl};

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// Vendor id shared by all virtio devices.
pub const VIRTIO_VENDOR: u16 = 0x1AF4;

/// One PCI function.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor: u16,
    pub id: u16,
}

/// Config mechanism 1 address: enable bit, bus/device/function, register.
fn address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | (u32::from(bus) << 16)
        | (u32::from(device) << 11)
        | (u32::from(function) << 8)
        | u32::from(offset & 0xFC)
}

/// Read a 32-bit configuration register.
pub fn read32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    // Safety: the documented PCI configuration mechanism 1 sequence — write
    // the address register, then read the data register it latches to.
    unsafe {
        outl(CONFIG_ADDRESS, address(bus, device, function, offset));
        inl(CONFIG_DATA)
    }
}

/// Read a 16-bit configuration register (the containing dword is shifted).
pub fn read16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let shift = (offset & 2) * 8;
    (read32(bus, device, function, offset) >> shift) as u16
}

/// Read an 8-bit configuration register.
pub fn read8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let shift = (offset & 3) * 8;
    (read32(bus, device, function, offset) >> shift) as u8
}

/// Read BAR `index` (0..=5) as the raw 32-bit register. Bit 0 tells I/O from
/// memory; a 64-bit BAR also occupies the next register.
pub fn bar(device: Device, index: u8) -> u32 {
    read32(device.bus, device.device, device.function, 0x10 + index * 4)
}

/// Visit every present PCI function. A function is present when its vendor id
/// is not 0xFFFF; a multi-function device (header type bit 7) is scanned per
/// function, a single-function device only as function 0.
///
/// The walk starts at bus 0 and follows PCI-to-PCI bridges to their secondary
/// buses instead of probing all 256 buses: every config read is two port
/// accesses (VM exits under a hypervisor), and the blind sweep was 16k of them
/// per pass, about 0.15 s of boot on WHPX.
pub fn for_each(mut visit: impl FnMut(Device)) {
    let mut seen = [false; 256];
    scan_bus(0, &mut seen, &mut visit);
}

/// Class/subclass of a PCI-to-PCI bridge.
const CLASS_BRIDGE_PCI: u16 = 0x0604;

fn scan_bus(bus: u8, seen: &mut [bool; 256], visit: &mut impl FnMut(Device)) {
    // A bridge misconfigured to point back at a visited bus must not loop.
    if core::mem::replace(&mut seen[bus as usize], true) {
        return;
    }
    for device in 0..32 {
        if read16(bus, device, 0, 0) == 0xFFFF {
            continue;
        }
        let header = read8(bus, device, 0, 0x0E);
        let functions = if header & 0x80 != 0 { 8 } else { 1 };
        for function in 0..functions {
            let vendor = read16(bus, device, function, 0);
            if vendor == 0xFFFF {
                continue;
            }
            visit(Device {
                bus,
                device,
                function,
                vendor,
                id: read16(bus, device, function, 2),
            });
            if read16(bus, device, function, 0x0A) == CLASS_BRIDGE_PCI {
                let secondary = read8(bus, device, function, 0x19);
                scan_bus(secondary, seen, visit);
            }
        }
    }
}

/// Find the function matching `vendor` and the earliest id in `ids` (the
/// caller's priority order), in a single enumeration pass.
pub fn find_any(vendor: u16, ids: &[u16]) -> Option<Device> {
    let mut best: Option<(usize, Device)> = None;
    for_each(|device| {
        if device.vendor != vendor {
            return;
        }
        if let Some(rank) = ids.iter().position(|id| *id == device.id) {
            if best.is_none_or(|(held, _)| rank < held) {
                best = Some((rank, device));
            }
        }
    });
    best.map(|(_, device)| device)
}
