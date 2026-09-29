//! PCI configuration-space access (issue #100; upgraded for #239).
//!
//! Legacy config mechanism 1 (0xCF8/0xCFC) is available on every x86 machine
//! LazyOS boots on, so there is no MMCONFIG support yet. On top of the raw
//! read/write primitives this exposes what the device core needs: per-function
//! enumeration, BAR decoding with the write-ones size probe (32- and 64-bit),
//! command-register control (memory/I/O decode, bus mastering), a bounded
//! capability-list walk, and the legacy interrupt-line read.

use super::{Bar, BarKind};
use crate::arch::io::{inl, outl};

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

/// Vendor id shared by all virtio devices.
pub const VIRTIO_VENDOR: u16 = 0x1AF4;

const REG_COMMAND: u8 = 0x04;
const REG_STATUS: u8 = 0x06;
const REG_REVISION: u8 = 0x08;
const REG_PROG_IF: u8 = 0x09;
const REG_SUBCLASS: u8 = 0x0A;
const REG_CLASS: u8 = 0x0B;
const REG_HEADER_TYPE: u8 = 0x0E;
const REG_BAR0: u8 = 0x10;
const REG_SUBSYSTEM: u8 = 0x2C;
const REG_CAP_PTR: u8 = 0x34;
const REG_INTERRUPT_LINE: u8 = 0x3C;
const REG_INTERRUPT_PIN: u8 = 0x3D;

/// Command-register bits this core manipulates.
pub const COMMAND_IO: u16 = 1 << 0;
pub const COMMAND_MEMORY: u16 = 1 << 1;
pub const COMMAND_BUS_MASTER: u16 = 1 << 2;
/// Command bit that stops the function asserting its INTx line.
pub const COMMAND_INTX_DISABLE: u16 = 1 << 10;

/// Status-register bit announcing a capability list.
const STATUS_CAPABILITIES: u16 = 1 << 4;

/// Cap on the capability-list walk so a malformed device cannot spin us.
const MAX_CAPABILITIES: usize = 48;

/// Bus/device/function coordinates of one PCI function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Address {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

/// A present PCI function and its identity registers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Function {
    pub address: Address,
    pub vendor: u16,
    pub id: u16,
}

/// Config mechanism 1 address: enable bit, bus/device/function, register.
fn config_address(address: Address, offset: u8) -> u32 {
    0x8000_0000
        | (u32::from(address.bus) << 16)
        | (u32::from(address.device) << 11)
        | (u32::from(address.function) << 8)
        | u32::from(offset & 0xFC)
}

/// Read a 32-bit configuration register.
pub fn read32(address: Address, offset: u8) -> u32 {
    let register = config_address(address, offset);
    // Safety: the documented PCI configuration mechanism 1 sequence — write
    // the address register, then read the data register it latches to. Port
    // I/O carries no Rust aliasing hazard (see `arch::io`).
    unsafe {
        outl(CONFIG_ADDRESS, register);
        inl(CONFIG_DATA)
    }
}

/// Read a 16-bit configuration register (the containing dword is shifted).
pub fn read16(address: Address, offset: u8) -> u16 {
    let shift = (offset & 2) * 8;
    (read32(address, offset) >> shift) as u16
}

/// Read an 8-bit configuration register.
pub fn read8(address: Address, offset: u8) -> u8 {
    let shift = (offset & 3) * 8;
    (read32(address, offset) >> shift) as u8
}

/// Write a 32-bit configuration register.
pub fn write32(address: Address, offset: u8, value: u32) {
    let register = config_address(address, offset);
    // Safety: same mechanism-1 sequence as `read32`, writing the latched data
    // register. The caller owns the protocol meaning of the write.
    unsafe {
        outl(CONFIG_ADDRESS, register);
        outl(CONFIG_DATA, value);
    }
}

/// Write a 16-bit configuration register via a dword read-modify-write.
pub fn write16(address: Address, offset: u8, value: u16) {
    let shift = (offset & 2) * 8;
    let old = read32(address, offset);
    let mask = !(0xFFFFu32 << shift);
    write32(address, offset, (old & mask) | (u32::from(value) << shift));
}

/// Read BAR register `index` (0..=5) raw. Bit 0 tells I/O from memory; a
/// 64-bit memory BAR also occupies the next register.
pub fn bar_raw(address: Address, index: u8) -> u32 {
    read32(address, REG_BAR0 + index * 4)
}

/// Decode a write-ones mask into a window length, or `None` for a mask that
/// describes no window. The low bits are type/flag bits and read as zero in
/// the mask; I/O BARs reserve two bits, memory BARs four. `is_64` selects the
/// address width: a 32-bit BAR cannot describe more than 4 GiB, so its mask's
/// high half must not be treated as implemented.
pub fn decode_size(mask: u64, io: bool, is_64: bool) -> Option<u64> {
    let clear = if io { 0x3 } else { 0xF };
    let mask = mask & !clear;
    if mask == 0 {
        return None;
    }
    Some(if is_64 {
        (!mask).wrapping_add(1)
    } else {
        u64::from((!mask as u32).wrapping_add(1))
    })
}

/// A device may implement only the low 16 address bits of an I/O BAR (the
/// x86 I/O space is 16 bits wide), so the upper half reads back as zero. Treat
/// those bits as set, or `decode_size` would compute a bogus 64 KiB+ length.
pub(crate) fn io_mask(mask: u32) -> u32 {
    if mask >> 16 == 0 {
        mask | 0xFFFF_0000
    } else {
        mask
    }
}

/// Run `sizing` with memory and I/O decode switched off, then restore the
/// command register. PCI requires decode to be disabled while a BAR holds the
/// all-ones sizing pattern, or the function (and any device it shadows) briefly
/// decodes a bogus window.
fn with_decode_off<T>(address: Address, sizing: impl FnOnce() -> T) -> T {
    let saved = command(address);
    write_command(address, saved & !(COMMAND_IO | COMMAND_MEMORY));
    let result = sizing();
    write_command(address, saved);
    result
}

/// Size an I/O BAR: save, write all-ones, read the mask, restore.
fn size_io(address: Address, index: u8) -> Option<u64> {
    let offset = REG_BAR0 + index * 4;
    let saved = read32(address, offset);
    write32(address, offset, 0xFFFF_FFFF);
    let mask = read32(address, offset);
    write32(address, offset, saved);
    decode_size(u64::from(io_mask(mask)), true, false)
}

/// Size a memory BAR (one or two registers): save, write all-ones to the
/// window's registers, read the masks, restore. Callers wrap this in
/// [`with_decode_off`] so the all-ones pattern is never live.
fn size_mem(address: Address, index: u8, is_64: bool) -> Option<u64> {
    let offset = REG_BAR0 + index * 4;
    let saved_lo = read32(address, offset);
    let saved_hi = if is_64 {
        Some(read32(address, offset + 4))
    } else {
        None
    };
    write32(address, offset, 0xFFFF_FFFF);
    if is_64 {
        write32(address, offset + 4, 0xFFFF_FFFF);
    }
    let mask_lo = read32(address, offset);
    let mask_hi = if is_64 {
        let hi = read32(address, offset + 4);
        write32(address, offset + 4, saved_hi.unwrap_or(0));
        hi
    } else {
        0
    };
    write32(address, offset, saved_lo);
    let mask = if is_64 {
        (u64::from(mask_hi) << 32) | u64::from(mask_lo)
    } else {
        u64::from(mask_lo)
    };
    decode_size(mask, false, is_64)
}

/// Read and decode BAR `index`. Returns the [`Bar`] and how many registers it
/// occupies (2 for a 64-bit memory BAR). `None` for an unimplemented BAR.
pub fn read_bar(address: Address, index: u8) -> Option<(Bar, u8)> {
    if index >= 6 {
        return None;
    }
    let raw = bar_raw(address, index);
    if raw == 0 {
        return None;
    }
    if raw & 1 == 1 {
        let bar = Bar {
            index,
            kind: BarKind::Io,
            base: u64::from(raw & !0x3),
            len: with_decode_off(address, || size_io(address, index)).unwrap_or(0),
            is_64: false,
            prefetchable: false,
        };
        return Some((bar, 1));
    }
    let type_bits = (raw >> 1) & 0x3;
    let is_64 = type_bits == 0x2;
    let mut base = u64::from(raw & !0xF);
    if is_64 {
        base |= u64::from(bar_raw(address, index + 1)) << 32;
    }
    let bar = Bar {
        index,
        kind: BarKind::Mem,
        base,
        len: with_decode_off(address, || size_mem(address, index, is_64)).unwrap_or(0),
        is_64,
        prefetchable: type_bits == 0x1,
    };
    Some((bar, if is_64 { 2 } else { 1 }))
}

/// The command register (memory/I/O decode, bus-master, ...).
pub fn command(address: Address) -> u16 {
    read16(address, REG_COMMAND)
}

/// Write the command register alone. The status half of the dword goes out as
/// zero: status bits are write-1-to-clear, so echoing back what a
/// read-modify-write just read would silently clear pending error bits.
pub fn write_command(address: Address, value: u16) {
    write32(address, REG_COMMAND, u32::from(value));
}

/// Set `bits` in the command register.
pub fn set_command(address: Address, bits: u16) {
    write_command(address, command(address) | bits);
}

/// Clear `bits` in the command register.
pub fn clear_command(address: Address, bits: u16) {
    write_command(address, command(address) & !bits);
}

pub fn enable_memory(address: Address) {
    set_command(address, COMMAND_MEMORY);
}

pub fn disable_memory(address: Address) {
    clear_command(address, COMMAND_MEMORY);
}

pub fn enable_io(address: Address) {
    set_command(address, COMMAND_IO);
}

pub fn disable_io(address: Address) {
    clear_command(address, COMMAND_IO);
}

pub fn enable_bus_master(address: Address) {
    set_command(address, COMMAND_BUS_MASTER);
}

pub fn disable_bus_master(address: Address) {
    clear_command(address, COMMAND_BUS_MASTER);
}

/// One entry of the capability list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capability {
    /// Capability id (`0x05` MSI, `0x10` PCIe, `0x11` MSI-X, virtio vendor caps, ...).
    pub id: u8,
    /// Config-space offset of the capability header.
    pub offset: u8,
}

/// Visit every capability in the function's list, bounded to
/// [`MAX_CAPABILITIES`] hops. Returns how many were seen.
pub fn for_each_capability(address: Address, mut visit: impl FnMut(Capability)) -> usize {
    if read16(address, REG_STATUS) & STATUS_CAPABILITIES == 0 {
        return 0;
    }
    let mut pointer = read8(address, REG_CAP_PTR) & 0xFC;
    let mut count = 0;
    while pointer != 0 && count < MAX_CAPABILITIES {
        visit(Capability {
            id: read8(address, pointer),
            offset: pointer,
        });
        count += 1;
        pointer = read8(address, pointer + 1) & 0xFC;
    }
    count
}

/// The legacy interrupt line the firmware assigned the function (0 when not
/// routed). The device core records it as the function's [`super::Irq`].
pub fn interrupt_line(address: Address) -> u8 {
    read8(address, REG_INTERRUPT_LINE)
}

/// The INTx pin the function uses: 0 none, 1-4 for INTA#-INTD#. A function with
/// no pin never raises a legacy interrupt whatever its Interrupt Line says.
pub fn interrupt_pin(address: Address) -> u8 {
    read8(address, REG_INTERRUPT_PIN)
}

/// Identity/class fields read straight from config space, for [`Function`]s
/// and the device core's `DeviceInfo` builder.
pub fn revision(address: Address) -> u8 {
    read8(address, REG_REVISION)
}

pub fn prog_if(address: Address) -> u8 {
    read8(address, REG_PROG_IF)
}

pub fn subclass(address: Address) -> u8 {
    read8(address, REG_SUBCLASS)
}

pub fn class(address: Address) -> u8 {
    read8(address, REG_CLASS)
}

pub fn header_type(address: Address) -> u8 {
    read8(address, REG_HEADER_TYPE)
}

pub fn subsystem_id(address: Address) -> (u16, u16) {
    (
        read16(address, REG_SUBSYSTEM),
        read16(address, REG_SUBSYSTEM + 2),
    )
}

/// Visit every present PCI function. A function is present when its vendor id
/// is not 0xFFFF; a multi-function device (header type bit 7) is scanned per
/// function, a single-function device only as function 0.
///
/// The walk starts at bus 0 and follows PCI-to-PCI bridges to their secondary
/// buses instead of probing all 256 buses: every config read is two port
/// accesses (VM exits under a hypervisor), and the blind sweep was 16k of them
/// per pass, about 0.15 s of boot on WHPX.
pub fn for_each(mut visit: impl FnMut(Function)) {
    let mut seen = [false; 256];
    scan_bus(0, &mut seen, &mut visit);
}

/// Class/subclass of a PCI-to-PCI bridge.
const CLASS_BRIDGE_PCI: u16 = 0x0604;

fn scan_bus(bus: u8, seen: &mut [bool; 256], visit: &mut impl FnMut(Function)) {
    // A bridge misconfigured to point back at a visited bus must not loop.
    if core::mem::replace(&mut seen[bus as usize], true) {
        return;
    }
    for device in 0..32 {
        let first = Address {
            bus,
            device,
            function: 0,
        };
        if read16(first, 0) == 0xFFFF {
            continue;
        }
        let functions = if header_type(first) & 0x80 != 0 { 8 } else { 1 };
        for function in 0..functions {
            let address = Address {
                bus,
                device,
                function,
            };
            let vendor = read16(address, 0);
            if vendor == 0xFFFF {
                continue;
            }
            visit(Function {
                address,
                vendor,
                id: read16(address, 2),
            });
            if read16(address, 0x0A) == CLASS_BRIDGE_PCI {
                scan_bus(read8(address, 0x19), seen, visit);
            }
        }
    }
}

/// Find the function matching `vendor` and the earliest id in `ids` (the
/// caller's priority order), in a single enumeration pass.
pub fn find_any(vendor: u16, ids: &[u16]) -> Option<Function> {
    let mut best: Option<(usize, Function)> = None;
    for_each(|function| {
        if function.vendor != vendor {
            return;
        }
        if let Some(rank) = ids.iter().position(|id| *id == function.id) {
            if best.is_none_or(|(held, _)| rank < held) {
                best = Some((rank, function));
            }
        }
    });
    best.map(|(_, function)| function)
}
