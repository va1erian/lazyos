//! ACPI table walker for the kernel's timer and power code
//! (`docs/real-pc-boot-plan.md`, phase H2).
//!
//! Firmware tables are hostile input: a real board can ship a wrong checksum,
//! a length that runs off the end of RAM, a root table with a thousand
//! entries or an MADT whose entry lengths do not add up. Everything here reads
//! physical memory through the [`PhysMem`] seam, checks each signature, length
//! and checksum before trusting a byte, bounds every loop by a constant, and
//! turns a bad table into an [`Error`] for that table alone: a broken HPET
//! table costs the HPET, not the FADT's PM timer.
//!
//! * [`Platform::discover`] walks RSDP -> XSDT (or RSDT) -> FADT, MADT, HPET,
//!   and validates the DSDT the FADT names, so a later `\_S5` scan has it.
//! * [`fadt`], [`madt`] and [`hpet`] decode the fields the kernel uses.
//!
//! Pure `no_std` logic with host tests; nothing here touches hardware.

#![no_std]

#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod fadt;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod gas;
pub mod hpet;
pub mod list;
pub mod madt;
pub mod sdt;
#[cfg(test)]
mod tests;

pub use fadt::{Fadt, PmTimer, ResetReg};
pub use gas::{AddressSpace, Gas};
pub use hpet::Hpet;
pub use madt::Madt;
pub use sdt::{Rsdp, Sdt};

/// Why a table (or the RSDP) was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No root table lists a table with this signature.
    NotFound,
    /// Some byte of the structure is outside readable physical memory.
    Unreadable,
    /// The signature is not the one the pointer promised.
    BadSignature,
    /// The bytes do not sum to zero.
    BadChecksum,
    /// The length field is too small for the structure or too large to trust.
    BadLength,
    /// A field or a sub-structure is inconsistent (an MADT entry that runs
    /// past the table, an address in the wrong address space, ...).
    Malformed,
}

/// Read-only access to physical memory. The kernel implements it over its
/// physical-memory mapping (refusing pages that are not mapped); the host
/// tests over a golden dump.
pub trait PhysMem {
    /// Copy `buf.len()` bytes starting at physical `addr` into `buf`. Returns
    /// false, leaving `buf` unspecified, when any byte is not readable.
    fn read(&self, addr: u64, buf: &mut [u8]) -> bool;
}

/// Little-endian field readers on top of [`PhysMem`].
pub(crate) trait PhysExt: PhysMem {
    fn bytes<const N: usize>(&self, addr: u64) -> Result<[u8; N], Error> {
        let mut buf = [0u8; N];
        if self.read(addr, &mut buf) {
            Ok(buf)
        } else {
            Err(Error::Unreadable)
        }
    }
    fn u8_at(&self, addr: u64) -> Result<u8, Error> {
        Ok(self.bytes::<1>(addr)?[0])
    }
    fn u16_at(&self, addr: u64) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.bytes(addr)?))
    }
    fn u32_at(&self, addr: u64) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.bytes(addr)?))
    }
    fn u64_at(&self, addr: u64) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.bytes(addr)?))
    }
}

impl<M: PhysMem + ?Sized> PhysExt for M {}

/// Everything [`Platform::discover`] found. Each table is `Err` on its own:
/// a refused MADT leaves the FADT usable.
#[derive(Clone, Debug)]
pub struct Platform {
    /// The validated RSDP.
    pub rsdp: Rsdp,
    /// The root table used (XSDT when valid, else RSDT).
    pub root: Sdt,
    /// Root entries that pointed at an unreadable or invalid table.
    pub bad_entries: u16,
    pub fadt: Result<Fadt, Error>,
    pub madt: Result<Madt, Error>,
    pub hpet: Result<Hpet, Error>,
    /// The DSDT the FADT names, header and checksum validated (for a later
    /// bounded `\_S5` scan). `NotFound` when the FADT is missing or names none.
    pub dsdt: Result<Sdt, Error>,
}

impl Platform {
    /// Walk the tables from the RSDP at `rsdp_addr`. Fails only when there is
    /// no usable RSDP or root table; every other problem is per table.
    pub fn discover(mem: &dyn PhysMem, rsdp_addr: u64) -> Result<Platform, Error> {
        let rsdp = Rsdp::read(mem, rsdp_addr)?;
        let root = sdt::root(mem, &rsdp)?;
        let mut platform = Platform {
            rsdp,
            root: root.table,
            bad_entries: 0,
            fadt: Err(Error::NotFound),
            madt: Err(Error::NotFound),
            hpet: Err(Error::NotFound),
            dsdt: Err(Error::NotFound),
        };
        for index in 0..root.entries {
            let Ok(addr) = root.entry(mem, index) else {
                platform.bad_entries += 1;
                continue;
            };
            platform.consider(mem, addr);
        }
        if let Ok(fadt) = &platform.fadt {
            if let Some(dsdt) = fadt.dsdt {
                platform.dsdt = Sdt::load(mem, dsdt, Some(*b"DSDT"), sdt::MAX_DSDT_LEN);
            }
        }
        Ok(platform)
    }

    /// Look at one root entry: the first valid table of each kind wins; a
    /// later duplicate is ignored, and a failure is kept only while no valid
    /// table of that kind has been seen.
    fn consider(&mut self, mem: &dyn PhysMem, addr: u64) {
        let Ok(signature) = mem.bytes::<4>(addr) else {
            self.bad_entries += 1;
            return;
        };
        match &signature {
            b"FACP" => keep_first(&mut self.fadt, || Fadt::parse(mem, addr)),
            b"APIC" => keep_first(&mut self.madt, || Madt::parse(mem, addr)),
            b"HPET" => keep_first(&mut self.hpet, || Hpet::parse(mem, addr)),
            _ => {}
        }
    }
}

fn keep_first<T>(slot: &mut Result<T, Error>, parse: impl FnOnce() -> Result<T, Error>) {
    if slot.is_err() {
        *slot = parse();
    }
}
