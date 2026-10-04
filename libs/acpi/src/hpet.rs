//! The HPET description table (signature `HPET`; IA-PC HPET specification
//! 1.0a, section 3.2.4) and the register layout the kernel reads.

use crate::gas::{AddressSpace, Gas};
use crate::sdt::{Sdt, MAX_TABLE_LEN};
use crate::{Error, PhysExt, PhysMem};

/// Register offsets in the HPET's 1 KiB MMIO block.
pub mod reg {
    /// General capabilities and ID; the period in femtoseconds is bits 32..64.
    pub const CAPABILITIES: u64 = 0x000;
    /// General configuration: bit 0 enables the main counter, bit 1 is the
    /// legacy replacement route (which takes IRQ0 away from the PIT).
    pub const CONFIG: u64 = 0x010;
    pub const MAIN_COUNTER: u64 = 0x0F0;
    pub const ENABLE_CNF: u64 = 1 << 0;
    pub const LEG_RT_CNF: u64 = 1 << 1;
}

/// Longest counter period the specification allows (100 ns), femtoseconds.
pub const MAX_PERIOD_FS: u32 = 100_000_000;

/// Size of the HPET register block.
pub const BLOCK_LEN: u64 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hpet {
    pub table: Sdt,
    /// Physical address of the register block (memory space, 1 KiB aligned).
    pub address: u64,
    /// `Event Timer Block ID`: revision, comparator count, counter size,
    /// legacy-route capability and PCI vendor ID.
    pub block_id: u32,
    pub number: u8,
    pub min_tick: u16,
}

impl Hpet {
    /// Validate and decode the HPET table at `phys`.
    pub fn parse(mem: &dyn PhysMem, phys: u64) -> Result<Hpet, Error> {
        let table = Sdt::load(mem, phys, Some(*b"HPET"), MAX_TABLE_LEN)?;
        if table.length < 56 {
            return Err(Error::BadLength);
        }
        let base = Gas::read(mem, phys + 40)?.ok_or(Error::Malformed)?;
        if base.space != AddressSpace::Memory || !base.address.is_multiple_of(BLOCK_LEN) {
            return Err(Error::Malformed);
        }
        base.address
            .checked_add(BLOCK_LEN)
            .ok_or(Error::Malformed)?;
        Ok(Hpet {
            table,
            address: base.address,
            block_id: mem.u32_at(phys + 36)?,
            number: mem.u8_at(phys + 52)?,
            min_tick: mem.u16_at(phys + 53)?,
        })
    }

    /// Comparators in the block (the ID's field is "count minus one").
    pub fn comparators(&self) -> u8 {
        ((self.block_id >> 8) & 0x1F) as u8 + 1
    }

    /// Whether the main counter is 64 bits wide.
    pub fn counter64(&self) -> bool {
        self.block_id & (1 << 13) != 0
    }
}

/// The counter period (femtoseconds) from a `CAPABILITIES` register value,
/// when it is within the specification (nonzero, at most 100 ns).
pub fn period_fs(capabilities: u64) -> Option<u32> {
    let period = (capabilities >> 32) as u32;
    (period != 0 && period <= MAX_PERIOD_FS).then_some(period)
}

/// The counter frequency (Hz) for a period in femtoseconds.
pub fn frequency(period_fs: u32) -> u64 {
    1_000_000_000_000_000 / u64::from(period_fs.max(1))
}
