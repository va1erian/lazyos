//! Programming a function's MSI and MSI-X capabilities (issue #616).
//!
//! Only the kernel writes these: the message address decides which memory
//! the device writes to when it interrupts, so it is never a driver's to
//! choose. `cfg_write` keeps refusing everything but the command register,
//! and `map_bar` leaves the MSI-X table's pages out of the driver's mapping.
//! The table is written through a kernel mapping made once per device.
//!
//! Every access here is PCI configuration (two port accesses that must not
//! be interleaved) or the table; callers run with interrupts off: syscalls,
//! and the bottom half (task context, or an interrupt that stopped code
//! holding no lock and doing no configuration access).

use spin::Mutex;

use crate::mem::mmio;

use super::errno::{Errno, ENOSYS};
use super::msi::Mode;
use super::pci::{self, Address, COMMAND_INTX_DISABLE, COMMAND_MEMORY};
use super::resources::{Msi, MsiX, Resources};
use super::table::MAX_DEVICES;
use super::{DeviceId, DeviceInfo};

/// The local APIC's message window; the destination APIC ID is bits 12..20.
const ADDRESS_BASE: u32 = 0xFEE0_0000;

const MSI_ENABLE: u16 = 1 << 0;
/// Multiple Message Enable (bits 4..7): always 0, one vector.
const MSI_MME: u16 = 0b111 << 4;
const MSIX_FUNCTION_MASK: u16 = 1 << 14;
const MSIX_ENABLE: u16 = 1 << 15;
/// Vector Control bit 0 of an MSI-X table entry: masked.
const ENTRY_MASKED: u32 = 1;

/// Kernel address of each device's MSI-X table (0: not mapped yet). A BAR
/// never moves, so one mapping per device serves every claim of it.
static TABLES: Mutex<[u64; MAX_DEVICES]> = Mutex::new([0; MAX_DEVICES]);

/// What one vector is programmed into.
#[derive(Clone, Copy, Debug)]
pub struct Hw {
    pub address: Address,
    kind: Kind,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Msi(Msi),
    /// The capability and the kernel address of table entry 0.
    MsiX(MsiX, u64),
}

impl Hw {
    pub fn mode(&self) -> Mode {
        match self.kind {
            Kind::Msi(_) => Mode::Msi,
            Kind::MsiX(..) => Mode::MsiX,
        }
    }
}

/// Program `info`'s function to send `vector` to APIC `dest`, masked where
/// the hardware can mask, and switch message interrupts on (INTx off).
pub fn enable(
    id: DeviceId,
    address: Address,
    info: &DeviceInfo,
    vector: u8,
    dest: u8,
) -> Result<Hw, Errno> {
    let message = ADDRESS_BASE | (u32::from(dest) << 12);
    let kind = if let Some(msi) = info.resources.msi() {
        program_msi(address, msi, message, vector);
        Kind::Msi(msi)
    } else if let Some(msix) = info.resources.msix() {
        let table = table_address(id, &info.resources, msix)?;
        program_msix(address, msix, table, message, vector);
        Kind::MsiX(msix, table)
    } else {
        return Err(ENOSYS);
    };
    pci::set_command(address, COMMAND_INTX_DISABLE);
    Ok(Hw { address, kind })
}

/// Config offset of the MSI data register and of the mask bits.
fn msi_offsets(msi: Msi) -> (u8, u8) {
    if msi.is_64 {
        (msi.cap + 0x0C, msi.cap + 0x10)
    } else {
        (msi.cap + 0x08, msi.cap + 0x0C)
    }
}

fn program_msi(address: Address, msi: Msi, message: u32, vector: u8) {
    let control = pci::read16(address, msi.cap + 2) & !(MSI_ENABLE | MSI_MME);
    pci::write16(address, msi.cap + 2, control);
    pci::write32(address, msi.cap + 4, message);
    if msi.is_64 {
        pci::write32(address, msi.cap + 8, 0);
    }
    let (data, mask) = msi_offsets(msi);
    pci::write16(address, data, u16::from(vector));
    if msi.maskable {
        pci::write32(address, mask, pci::read32(address, mask) | 1);
    }
    pci::write16(address, msi.cap + 2, control | MSI_ENABLE);
}

fn program_msix(address: Address, msix: MsiX, table: u64, message: u32, vector: u8) {
    let control = pci::read16(address, msix.cap + 2);
    // Function-mask while the table changes, and enable so it is in use.
    pci::write16(
        address,
        msix.cap + 2,
        control | MSIX_ENABLE | MSIX_FUNCTION_MASK,
    );
    with_memory_decode(address, || {
        for entry in 0..u64::from(msix.table_size) {
            write_table(table + entry * 16 + 12, ENTRY_MASKED);
        }
        write_table(table, message);
        write_table(table + 4, 0);
        write_table(table + 8, u32::from(vector));
    });
    pci::write16(
        address,
        msix.cap + 2,
        (control | MSIX_ENABLE) & !MSIX_FUNCTION_MASK,
    );
}

/// Mask or unmask the vector at the function (MSI without mask bits cannot).
pub fn mask(hw: &Hw, masked: bool) {
    match hw.kind {
        Kind::Msi(msi) if msi.maskable => {
            let (_, offset) = msi_offsets(msi);
            let bits = pci::read32(hw.address, offset);
            let next = if masked { bits | 1 } else { bits & !1 };
            if next != bits {
                pci::write32(hw.address, offset, next);
            }
        }
        Kind::Msi(_) => {}
        Kind::MsiX(_, table) => with_memory_decode(hw.address, || {
            write_table(table + 12, if masked { ENTRY_MASKED } else { 0 });
        }),
    }
}

/// Switch the function's message interrupts off (the vector is being freed).
pub fn disable(hw: &Hw) {
    mask(hw, true);
    match hw.kind {
        Kind::Msi(msi) => clear_bits(hw.address, msi.cap, MSI_ENABLE),
        Kind::MsiX(msix, _) => clear_bits(hw.address, msix.cap, MSIX_ENABLE),
    }
}

/// Clear both capabilities' enable bits, whatever programmed them.
pub fn disable_capabilities(address: Address, resources: &Resources) {
    if let Some(msi) = resources.msi() {
        clear_bits(address, msi.cap, MSI_ENABLE);
    }
    if let Some(msix) = resources.msix() {
        clear_bits(address, msix.cap, MSIX_ENABLE);
    }
}

fn clear_bits(address: Address, cap: u8, bits: u16) {
    let control = pci::read16(address, cap + 2);
    if control & bits != 0 {
        pci::write16(address, cap + 2, control & !bits);
    }
}

/// Run `write` with the function's memory decode on: the table is in a
/// memory BAR, and a claim may have it off (a fresh claim is quiesced, a
/// freed DMA buffer quiesces it). The command register is put back after.
fn with_memory_decode(address: Address, write: impl FnOnce()) {
    let command = pci::command(address);
    if command & COMMAND_MEMORY == 0 {
        pci::write_command(address, command | COMMAND_MEMORY);
    }
    write();
    if command & COMMAND_MEMORY == 0 {
        pci::write_command(address, command);
    }
}

fn write_table(at: u64, value: u32) {
    // SAFETY: `at` lies inside the table `table_address` mapped uncached for
    // this device (entry offsets are below `table_size * 16`), 4-aligned.
    unsafe { (at as *mut u32).write_volatile(value) };
}

/// The kernel address of `id`'s MSI-X table, mapping it on first use.
/// Discovery (`bus::check_msix`) already proved it lies inside the BAR.
fn table_address(id: DeviceId, resources: &Resources, msix: MsiX) -> Result<u64, Errno> {
    let mut tables = TABLES.lock();
    let slot = tables.get_mut(usize::from(id.0)).ok_or(ENOSYS)?;
    if *slot == 0 {
        let bar = resources.bar(msix.table_bar).ok_or(ENOSYS)?;
        let phys = bar.base + u64::from(msix.table_offset);
        let page = phys & !0xFFF;
        let len = (phys + msix.table_len()).next_multiple_of(4096) - page;
        let va = mmio::map_kernel(page, len).map_err(|_| ENOSYS)?;
        *slot = va + (phys - page);
    }
    Ok(*slot)
}
