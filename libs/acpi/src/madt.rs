//! The Multiple APIC Description Table (signature `APIC`; ACPI 6.5, 5.2.12).
//!
//! The kernel uses the local APIC address, the I/O APICs and the interrupt
//! source overrides (issue #616: ISA lines on the I/O APIC, with the trigger
//! and polarity [`inti`] decodes from an override's flags). An
//! entry list that does not add up (a zero or short entry length, an entry
//! running past the table) refuses the whole MADT: a table that lies about
//! its own layout cannot be trusted about addresses either.

use crate::list::List;
use crate::sdt::{Sdt, HEADER_LEN, MAX_TABLE_LEN};
use crate::{Error, PhysExt, PhysMem};

/// I/O APICs recorded at most (servers have a handful).
pub const MAX_IOAPICS: usize = 16;
/// Interrupt source overrides recorded at most (ISA has 16 lines).
pub const MAX_OVERRIDES: usize = 32;
/// Local APIC NMI entries recorded at most.
pub const MAX_LAPIC_NMIS: usize = 16;
/// Entries examined at most (an 8 KiB MADT of 8-byte entries is 1024).
const MAX_ENTRIES: u32 = 8192;

/// `Flags` bit: the machine also has dual 8259 PICs.
pub const PCAT_COMPAT: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IoApic {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
}

/// An ISA interrupt that is wired to a different GSI (or polarity/trigger).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Override {
    pub bus: u8,
    pub source: u8,
    pub gsi: u32,
    /// MPS INTI flags: polarity in bits 0-1, trigger mode in bits 2-3.
    pub flags: u16,
}

/// Which LINT pin of which processor carries NMI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LapicNmi {
    /// ACPI processor UID (0xFF / 0xFFFF_FFFF: all processors).
    pub processor: u32,
    pub flags: u16,
    pub lint: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Madt {
    pub table: Sdt,
    /// The local APIC physical address (the 64-bit override when present).
    pub lapic_address: u64,
    pub flags: u32,
    /// Enabled (or online-capable) processors, xAPIC and x2APIC entries.
    pub processors: u32,
    /// APIC ID of the first enabled processor entry (normally the BSP).
    pub first_apic_id: Option<u32>,
    pub ioapics: List<IoApic, MAX_IOAPICS>,
    pub overrides: List<Override, MAX_OVERRIDES>,
    pub lapic_nmis: List<LapicNmi, MAX_LAPIC_NMIS>,
}

impl Madt {
    /// Validate and decode the MADT at `phys`.
    pub fn parse(mem: &dyn PhysMem, phys: u64) -> Result<Madt, Error> {
        let table = Sdt::load(mem, phys, Some(*b"APIC"), MAX_TABLE_LEN)?;
        if table.length < HEADER_LEN + 8 {
            return Err(Error::BadLength);
        }
        let mut madt = Madt {
            table,
            lapic_address: u64::from(mem.u32_at(phys + 36)?),
            flags: mem.u32_at(phys + 40)?,
            processors: 0,
            first_apic_id: None,
            ioapics: List::default(),
            overrides: List::default(),
            lapic_nmis: List::default(),
        };
        let mut offset = HEADER_LEN + 8;
        let mut examined = 0;
        while offset < table.length {
            examined += 1;
            if examined > MAX_ENTRIES || table.length - offset < 2 {
                return Err(Error::Malformed);
            }
            let at = phys + u64::from(offset);
            let [kind, len] = mem.bytes::<2>(at)?;
            let len = u32::from(len);
            if len < 2 || len > table.length - offset {
                return Err(Error::Malformed);
            }
            madt.entry(mem, at, kind, len)?;
            offset += len;
        }
        if madt.lapic_address == 0 || !madt.lapic_address.is_multiple_of(4096) {
            return Err(Error::Malformed);
        }
        Ok(madt)
    }

    /// Decode one entry of `kind` and `len` bytes at `at`. Unknown kinds are
    /// skipped; a known kind shorter than its layout is malformed.
    fn entry(&mut self, mem: &dyn PhysMem, at: u64, kind: u8, len: u32) -> Result<(), Error> {
        let need = match kind {
            0 => 8,
            1 => 12,
            2 => 10,
            4 => 6,
            5 => 12,
            9 => 16,
            10 => 12,
            _ => return Ok(()),
        };
        if len < need {
            return Err(Error::Malformed);
        }
        match kind {
            // Processor local APIC: UID, APIC ID, flags.
            0 => self.processor(u32::from(mem.u8_at(at + 3)?), mem.u32_at(at + 4)?),
            1 => {
                self.ioapics.push(IoApic {
                    id: mem.u8_at(at + 2)?,
                    address: mem.u32_at(at + 4)?,
                    gsi_base: mem.u32_at(at + 8)?,
                });
            }
            2 => {
                self.overrides.push(Override {
                    bus: mem.u8_at(at + 2)?,
                    source: mem.u8_at(at + 3)?,
                    gsi: mem.u32_at(at + 4)?,
                    flags: mem.u16_at(at + 8)?,
                });
            }
            4 => {
                let uid = mem.u8_at(at + 2)?;
                self.lapic_nmis.push(LapicNmi {
                    processor: if uid == 0xFF {
                        u32::MAX
                    } else {
                        u32::from(uid)
                    },
                    flags: mem.u16_at(at + 3)?,
                    lint: mem.u8_at(at + 5)?,
                });
            }
            5 => {
                let address = mem.u64_at(at + 4)?;
                if address != 0 {
                    self.lapic_address = address;
                }
            }
            // Processor local x2APIC: reserved, x2APIC ID, flags, UID.
            9 => self.processor(mem.u32_at(at + 4)?, mem.u32_at(at + 8)?),
            10 => {
                self.lapic_nmis.push(LapicNmi {
                    processor: mem.u32_at(at + 4)?,
                    flags: mem.u16_at(at + 2)?,
                    lint: mem.u8_at(at + 8)?,
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Count a processor entry whose flags say enabled (bit 0) or
    /// online-capable (bit 1).
    fn processor(&mut self, apic_id: u32, flags: u32) {
        if flags & 0b11 != 0 {
            self.processors = self.processors.saturating_add(1);
            self.first_apic_id.get_or_insert(apic_id);
        }
    }

    /// Whether the machine has the dual-8259 PIC the kernel uses.
    pub fn pcat_compat(&self) -> bool {
        self.flags & PCAT_COMPAT != 0
    }

    /// The I/O APIC whose inputs include `gsi`, given each one's input count
    /// (`pins`, read from the chip's version register: the MADT does not say).
    pub fn ioapic_for(&self, gsi: u32, pins: impl Fn(&IoApic) -> u32) -> Option<IoApic> {
        self.ioapics
            .as_slice()
            .iter()
            .find(|io| gsi >= io.gsi_base && gsi - io.gsi_base < pins(io))
            .copied()
    }

    /// The GSI and INTI flags of ISA line `irq` (identity when no override).
    pub fn isa_gsi(&self, irq: u8) -> (u32, u16) {
        self.overrides
            .as_slice()
            .iter()
            .find(|o| o.bus == 0 && o.source == irq)
            .map_or((u32::from(irq), 0), |o| (o.gsi, o.flags))
    }
}

/// How an interrupt input is signalled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signal {
    /// Level-triggered (else edge).
    pub level: bool,
    /// Asserted low (else high).
    pub active_low: bool,
}

impl Signal {
    /// ISA interrupts: edge-triggered, active high.
    pub const ISA: Signal = Signal {
        level: false,
        active_low: false,
    };
    /// PCI INTx: level-triggered, active low.
    pub const PCI: Signal = Signal {
        level: true,
        active_low: true,
    };
}

/// Decode MPS INTI `flags` (polarity in bits 0-1, trigger in bits 2-3; 0
/// "conforms to the bus", 1 high/edge, 3 low/level) over the bus default.
/// The reserved value 2 keeps the bus default rather than guessing.
pub fn inti(flags: u16, bus: Signal) -> Signal {
    let active_low = match flags & 0b11 {
        0b01 => false,
        0b11 => true,
        _ => bus.active_low,
    };
    let level = match (flags >> 2) & 0b11 {
        0b01 => false,
        0b11 => true,
        _ => bus.level,
    };
    Signal { level, active_low }
}
