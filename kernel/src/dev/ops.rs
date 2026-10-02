//! Resource operations of the device syscall (issue #240): `map_bar`, `pio`
//! and PCI configuration access.
//!
//! Every operation takes a `Device` handle, never an address. What it may touch
//! is looked up in the device's own enumerated resources and bounds-checked
//! with checked arithmetic; the handle's right for that resource family and the
//! claim generation are verified first by [`super::syscall::resolve`].

use spin::Mutex;
use x86_64::PhysAddr;

use crate::arch::io;
use crate::ipc::handles::rights;
use crate::ipc::shared_va::Allocator;
use crate::mem::mmio;
use crate::quota::{self, Resource};

use super::claims::{Mapping, CLAIMS};
use super::errno::{Errno, EBUSY, EDQUOT, EINVAL, ENOMEM, EPERM};
use super::pci::{self, COMMAND_BUS_MASTER, COMMAND_INTX_DISABLE, COMMAND_IO, COMMAND_MEMORY};
use super::report::{self, reason};
use super::resources::MAX_BARS;
use super::syscall::Resolved;
use super::{BarKind, BusId};

const PAGE: u64 = 4096;

/// Userspace virtual range MMIO mappings come from. It lies in PML4 entry 0
/// (below 512 GiB), the per-process user half: unlike the shared-buffer range
/// (`ipc::shared_va`), whose page-table subtree is one object shared by every
/// address space, a mapping here exists only in the claimant's own tables.
pub const MMIO_VA_BASE: u64 = 0x0000_0030_0000_0000;
/// End of the MMIO range (exclusive): 32 GiB of address space.
pub const MMIO_VA_END: u64 = 0x0000_0038_0000_0000;
/// Largest BAR `map_bar` will map. Bigger windows (a GPU aperture) need their
/// own design; this bounds page-table memory a single call can consume.
pub const MAX_MAP_BYTES: u64 = 64 << 20;

static MMIO_VA: Mutex<Allocator> = Mutex::new(Allocator::new(MMIO_VA_BASE));

/// The address space the current syscall runs in: a syscall executes on its
/// caller's page tables, so the active table is the caller's.
pub(super) fn current_table() -> PhysAddr {
    crate::mem::kernel_table()
}

/// Give a VA range back (teardown and rollback).
pub(super) fn release_va(va: u64, pages: u64) {
    MMIO_VA.lock().release(va, pages * PAGE);
}

/// `map_bar(handle, bar)`: map a memory BAR uncached into the caller, returning
/// its virtual address.
///
/// Refused: a BAR that is not memory, is under a page (a page-granular mapping
/// would expose registers of a neighbouring function), is larger than
/// [`MAX_MAP_BYTES`], is unassigned, wraps the address space, lies past the
/// CPU's physical address width, or overlaps RAM (a misprogrammed BAR must
/// never hand a driver the kernel's memory). One mapping per BAR. The quota is
/// charged by uid, not through the per-address-space ledger, so a driver
/// cannot dodge it by exiting its address space early.
pub fn map_bar(r: &Resolved, index: u64) -> Result<u64, Errno> {
    if r.rights & rights::DEV_MMIO == 0 {
        return Err(EPERM);
    }
    let index = u8::try_from(index)
        .ok()
        .filter(|&index| usize::from(index) < MAX_BARS)
        .ok_or(EINVAL)?;
    let bar = r.info.resources.bar(index).ok_or(EINVAL)?;
    if bar.kind != BarKind::Mem
        || bar.base == 0
        || bar.base & (PAGE - 1) != 0
        || bar.len < PAGE
        || bar.len > MAX_MAP_BYTES
        || bar.len & (PAGE - 1) != 0
    {
        return Err(EINVAL);
    }
    let end = bar.base.checked_add(bar.len).ok_or(EINVAL)?;
    // A 64-bit BAR above 4 GiB is fine (the mapping path has no 32-bit
    // assumption); one past what the CPU can address is not.
    if end > mmio::phys_limit() {
        return Err(EINVAL);
    }
    if mmio::overlaps_ram(bar.base, end) {
        report::record(
            r.claim.owner,
            &r.info,
            super::class::method::MAP,
            false,
            reason::BAR_IN_RAM,
        );
        return Err(EPERM);
    }
    if r.claim.maps[usize::from(index)].is_some() {
        return Err(EBUSY);
    }
    let pages = bar.len / PAGE;
    let va = MMIO_VA.lock().alloc(bar.len);
    if va < MMIO_VA_BASE || va.checked_add(bar.len).is_none_or(|top| top > MMIO_VA_END) {
        release_va(va, pages);
        return Err(ENOMEM);
    }
    // Quota last among the fallible steps that need no undo of their own.
    if quota::charge(r.claim.uid, Resource::UserMemory, bar.len).is_err() {
        release_va(va, pages);
        return Err(EDQUOT);
    }
    let table = current_table();
    if !mmio::map_mmio(table, va, bar.base, pages) {
        quota::release(r.claim.uid, Resource::UserMemory, bar.len);
        release_va(va, pages);
        return Err(ENOMEM);
    }
    let mapping = Mapping {
        table: table.as_u64(),
        va,
        phys: bar.base,
        pages,
    };
    let recorded = {
        let mut claims = CLAIMS.lock();
        match claims.get_mut(r.id) {
            Some(claim) if claim.generation == r.claim.generation => {
                claim.maps[usize::from(index)] = Some(mapping);
                true
            }
            _ => false,
        }
    };
    if !recorded {
        mmio::unmap_mmio(table, va, bar.base, pages);
        quota::release(r.claim.uid, Resource::UserMemory, bar.len);
        release_va(va, pages);
        return Err(EINVAL);
    }
    Ok(va)
}

/// Port-I/O windows a driver may never reach even through a device's own BAR:
/// everything below 0x100 (PIC, PIT, keyboard controller, CMOS, DMA, POST) and
/// the PCI configuration mechanism at 0xCF8-0xCFF. A BAR that claims such ports
/// is a hardware quirk or an attack, not a device to hand over.
fn port_forbidden(port: u32, width: u32) -> bool {
    let last = port + width - 1;
    port < 0x100 || (port <= 0xCFF && last >= 0xCF8)
}

/// Decoded `pio` request word: `width | write << 8 | value << 32`.
struct PioRequest {
    width: u32,
    write: bool,
    value: u32,
}

fn decode_pio(packed: u64) -> Result<PioRequest, Errno> {
    let width = (packed & 0xFF) as u32;
    let write = (packed >> 8) & 1 != 0;
    // Bits 9..31 are reserved: refuse rather than silently ignore them.
    if !matches!(width, 1 | 2 | 4) || (packed >> 9) & 0x7F_FFFF != 0 {
        return Err(EINVAL);
    }
    Ok(PioRequest {
        width,
        write,
        value: (packed >> 32) as u32,
    })
}

/// `pio(handle, bar, offset, packed)`: one port read or write inside the
/// device's own I/O BAR. Returns the value read (0 for a write).
pub fn pio(r: &Resolved, index: u64, offset: u64, packed: u64) -> Result<u64, Errno> {
    if r.rights & rights::DEV_PIO == 0 {
        return Err(EPERM);
    }
    let request = decode_pio(packed)?;
    let index = u8::try_from(index).map_err(|_| EINVAL)?;
    let bar = r.info.resources.bar(index).ok_or(EINVAL)?;
    if bar.kind != BarKind::Io || bar.len == 0 {
        return Err(EINVAL);
    }
    let width = u64::from(request.width);
    if offset.checked_add(width).is_none_or(|end| end > bar.len) || !offset.is_multiple_of(width) {
        return Err(EINVAL);
    }
    let port = bar.base.checked_add(offset).ok_or(EINVAL)?;
    if port.checked_add(width).is_none_or(|end| end > 0x1_0000) {
        return Err(EINVAL);
    }
    let port = port as u32;
    if port_forbidden(port, request.width) {
        return Err(EPERM);
    }
    let port = port as u16;
    // SAFETY: `port..port + width` lies inside the claimed device's own I/O BAR
    // (checked above with overflow-safe arithmetic), outside the forbidden
    // legacy and PCI-config ranges, and naturally aligned for `width`.
    unsafe {
        Ok(match (request.width, request.write) {
            (1, false) => u64::from(io::inb(port)),
            (2, false) => u64::from(io::inw(port)),
            (_, false) => u64::from(io::inl(port)),
            (1, true) => {
                io::outb(port, request.value as u8);
                0
            }
            (2, true) => {
                io::outw(port, request.value as u16);
                0
            }
            (_, true) => {
                io::outl(port, request.value);
                0
            }
        })
    }
}

/// Size of the standard PCI configuration space.
const CONFIG_SPACE: u64 = 256;
const REG_COMMAND: u64 = 0x04;

fn pci_address(r: &Resolved) -> Result<pci::Address, Errno> {
    match r.info.bus {
        BusId::Pci(address) => Ok(address),
        BusId::Platform => Err(EINVAL),
    }
}

/// Validate a config access: aligned, 1/2/4 bytes, inside the 256-byte space.
fn check_config(offset: u64, width: u64) -> Result<u8, Errno> {
    if !matches!(width, 1 | 2 | 4) || !offset.is_multiple_of(width) {
        return Err(EINVAL);
    }
    if offset
        .checked_add(width)
        .is_none_or(|end| end > CONFIG_SPACE)
    {
        return Err(EINVAL);
    }
    Ok(offset as u8)
}

/// `cfg_read(handle, offset, width)`: read the device's own config space.
pub fn cfg_read(r: &Resolved, offset: u64, width: u64) -> Result<u64, Errno> {
    if r.rights & rights::DEV_CONFIG == 0 {
        return Err(EPERM);
    }
    let address = pci_address(r)?;
    let offset = check_config(offset, width)?;
    Ok(match width {
        1 => u64::from(pci::read8(address, offset)),
        2 => u64::from(pci::read16(address, offset)),
        _ => u64::from(pci::read32(address, offset)),
    })
}

/// Command-register bits userspace may change.
const COMMAND_USER_MASK: u16 =
    COMMAND_IO | COMMAND_MEMORY | COMMAND_BUS_MASTER | COMMAND_INTX_DISABLE;

/// `cfg_write(handle, offset, width, value)`: the only writable register is
/// the 16-bit command register at 0x04, and only its decode, bus-master and
/// INTx-disable bits.
///
/// * Setting bus-master needs the `DMA` right (clearing never does).
/// * INTx-disable stays set until `irq_enable` armed the claim: a claim that
///   listens for no interrupt must not let its device assert a shared line.
/// * Every other bit keeps its current value, and status bits are never
///   written (see `pci::write_command`).
///
/// Everything else (BARs, interrupt line, capabilities) is refused: BAR
/// programming would let a driver place its device over RAM.
pub fn cfg_write(r: &Resolved, offset: u64, width: u64, value: u64) -> Result<u64, Errno> {
    if r.rights & rights::DEV_CONFIG == 0 {
        return Err(EPERM);
    }
    let address = pci_address(r)?;
    check_config(offset, width)?;
    if offset != REG_COMMAND || width != 2 {
        return Err(EPERM);
    }
    let value = u16::try_from(value).map_err(|_| EINVAL)?;
    let current = pci::command(address);
    let mut next = (current & !COMMAND_USER_MASK) | (value & COMMAND_USER_MASK);
    if value & COMMAND_BUS_MASTER != 0 && r.rights & rights::DEV_DMA == 0 {
        report::record(
            r.claim.owner,
            &r.info,
            super::class::method::DMA,
            false,
            reason::DMA_DENIED,
        );
        return Err(EPERM);
    }
    if !r.claim.armed {
        next |= COMMAND_INTX_DISABLE;
    }
    pci::write_command(address, next);
    Ok(0)
}
