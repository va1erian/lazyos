//! The Fixed ACPI Description Table (signature `FACP`; ACPI 6.5, 5.2.9).
//!
//! An ACPI 1.0 FADT is 116 bytes; every later field is read only when the
//! table's length covers it, and an extended (`X_`) block wins over the
//! legacy 32-bit one only when it is present.

use crate::gas::{AddressSpace, Gas};
use crate::sdt::{Sdt, MAX_TABLE_LEN};
use crate::{Error, PhysExt, PhysMem};

/// ACPI PM timer frequency, Hz.
pub const PM_TIMER_HZ: u64 = 3_579_545;

/// `Flags` bits.
pub mod flags {
    /// The PM timer is 32 bits wide (else 24).
    pub const TMR_VAL_EXT: u32 = 1 << 8;
    /// `RESET_REG` is supported.
    pub const RESET_REG_SUP: u32 = 1 << 10;
    /// Hardware-reduced ACPI: no PM timer, no PM1 blocks.
    pub const HW_REDUCED_ACPI: u32 = 1 << 20;
}

/// `IAPC_BOOT_ARCH` bits (zero in an ACPI 1.0 FADT, which lacks the field).
pub mod boot_arch {
    pub const LEGACY_DEVICES: u16 = 1 << 0;
    pub const I8042: u16 = 1 << 1;
    pub const VGA_NOT_PRESENT: u16 = 1 << 2;
    pub const MSI_NOT_SUPPORTED: u16 = 1 << 3;
    pub const CMOS_RTC_NOT_PRESENT: u16 = 1 << 5;
}

/// The ACPI power-management timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PmTimer {
    /// An I/O port or a memory address, 4 bytes wide.
    pub block: Gas,
    /// 32-bit counter (`TMR_VAL_EXT`); otherwise only bits 0..24 count.
    pub width32: bool,
}

impl PmTimer {
    /// Counter mask for this timer's width.
    pub fn mask(&self) -> u32 {
        if self.width32 {
            u32::MAX
        } else {
            0x00FF_FFFF
        }
    }

    /// Ticks from `earlier` to `later`, allowing one wrap of the counter.
    pub fn elapsed(&self, earlier: u32, later: u32) -> u32 {
        later.wrapping_sub(earlier) & self.mask()
    }
}

/// The reset register and the value to write to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetReg {
    pub register: Gas,
    pub value: u8,
}

/// The FADT fields the kernel uses (timer, reset, sleep control, DSDT).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fadt {
    pub table: Sdt,
    /// The FADT minor version (offset 131; 0 when absent).
    pub minor_version: u8,
    pub sci_interrupt: u16,
    pub smi_command: u32,
    pub acpi_enable: u8,
    pub acpi_disable: u8,
    pub pm1a_event: Option<Gas>,
    pub pm1b_event: Option<Gas>,
    /// PM1a control: where `SLP_TYP`/`SLP_EN` are written for S5.
    pub pm1a_control: Option<Gas>,
    pub pm1b_control: Option<Gas>,
    pub pm_timer: Option<PmTimer>,
    /// CMOS index of the RTC century register (0 when none).
    pub century: u8,
    pub iapc_boot_arch: u16,
    pub flags: u32,
    /// Present only when `RESET_REG_SUP` is set and the register is usable.
    pub reset: Option<ResetReg>,
    /// Physical address of the DSDT (`X_DSDT` when set, else `DSDT`).
    pub dsdt: Option<u64>,
    /// Physical address of the FACS (`X_FIRMWARE_CTRL` when set).
    pub facs: Option<u64>,
}

/// Smallest FADT accepted: the ACPI 1.0 layout.
const MIN_LEN: u32 = 116;

impl Fadt {
    /// Validate and decode the FADT at `phys`.
    pub fn parse(mem: &dyn PhysMem, phys: u64) -> Result<Fadt, Error> {
        let table = Sdt::load(mem, phys, Some(*b"FACP"), MAX_TABLE_LEN)?;
        if table.length < MIN_LEN {
            return Err(Error::BadLength);
        }
        let r = Reader { mem, table };
        let flags = r.u32(112)?;
        let mut fadt = Fadt {
            table,
            minor_version: r.u8(131)?.unwrap_or(0),
            sci_interrupt: mem.u16_at(phys + 46)?,
            smi_command: mem.u32_at(phys + 48)?,
            acpi_enable: mem.u8_at(phys + 52)?,
            acpi_disable: mem.u8_at(phys + 53)?,
            pm1a_event: r.block(148, 56, 88)?,
            pm1b_event: r.block(160, 60, 88)?,
            pm1a_control: r.block(172, 64, 89)?,
            pm1b_control: r.block(184, 68, 89)?,
            pm_timer: None,
            century: mem.u8_at(phys + 108)?,
            iapc_boot_arch: if table.revision >= 2 {
                mem.u16_at(phys + 109)?
            } else {
                0
            },
            flags,
            reset: None,
            dsdt: r.address(140, 40)?,
            facs: r.address(132, 36)?,
        };
        fadt.pm_timer = r.pm_timer(flags)?;
        if flags & flags::RESET_REG_SUP != 0 {
            fadt.reset = r.reset()?;
        }
        Ok(fadt)
    }

    /// Hardware-reduced ACPI (no PM timer, no fixed hardware).
    pub fn hw_reduced(&self) -> bool {
        self.flags & flags::HW_REDUCED_ACPI != 0
    }
}

/// Length-checked field access to one FADT.
struct Reader<'a> {
    mem: &'a dyn PhysMem,
    table: Sdt,
}

impl Reader<'_> {
    fn u8(&self, offset: u32) -> Result<Option<u8>, Error> {
        match self.table.field(offset, 1) {
            Some(addr) => Ok(Some(self.mem.u8_at(addr)?)),
            None => Ok(None),
        }
    }

    fn u32(&self, offset: u32) -> Result<u32, Error> {
        let addr = self.table.field(offset, 4).ok_or(Error::BadLength)?;
        self.mem.u32_at(addr)
    }

    fn gas(&self, offset: u32) -> Result<Option<Gas>, Error> {
        match self.table.field(offset, Gas::LEN) {
            Some(addr) => Gas::read(self.mem, addr),
            None => Ok(None),
        }
    }

    /// The `X_` block at `x_offset` when present, else the legacy port at
    /// `port_offset` with the byte length at `len_offset`.
    fn block(
        &self,
        x_offset: u32,
        port_offset: u32,
        len_offset: u32,
    ) -> Result<Option<Gas>, Error> {
        if let Some(gas) = self.gas(x_offset)? {
            return Ok(Some(gas));
        }
        let len = self.u8(len_offset)?.unwrap_or(0);
        Ok(Gas::io(self.u32(port_offset)?, len))
    }

    /// The 64-bit address at `x_offset` when nonzero, else the 32-bit one.
    fn address(&self, x_offset: u32, offset: u32) -> Result<Option<u64>, Error> {
        if let Some(addr) = self.table.field(x_offset, 8) {
            let x = self.mem.u64_at(addr)?;
            if x != 0 {
                return Ok(Some(x));
            }
        }
        let legacy = self.u32(offset)?;
        Ok((legacy != 0).then_some(u64::from(legacy)))
    }

    /// The PM timer: `X_PM_TMR_BLK` (I/O or memory) or `PM_TMR_BLK` with
    /// `PM_TMR_LEN == 4`. Anything else (another address space, a wrong
    /// width, a port that would wrap) means "no PM timer".
    fn pm_timer(&self, flags: u32) -> Result<Option<PmTimer>, Error> {
        if flags & flags::HW_REDUCED_ACPI != 0 {
            return Ok(None);
        }
        let block = match self.gas(208)? {
            Some(gas) => Some(gas),
            None if self.u8(91)? == Some(4) => Gas::io(self.u32(76)?, 4),
            None => None,
        };
        let usable = block.filter(|gas| match gas.space {
            AddressSpace::Io => gas.port(4).is_some(),
            AddressSpace::Memory => gas.address.is_multiple_of(4),
            _ => false,
        });
        Ok(usable.map(|block| PmTimer {
            block,
            width32: flags & flags::TMR_VAL_EXT != 0,
        }))
    }

    /// `RESET_REG` and `RESET_VALUE`, when the table is long enough and the
    /// register is in memory, I/O or PCI configuration space.
    fn reset(&self) -> Result<Option<ResetReg>, Error> {
        let Some(register) = self.gas(116)? else {
            return Ok(None);
        };
        let Some(value) = self.u8(128)? else {
            return Ok(None);
        };
        let usable = match register.space {
            AddressSpace::Io => register.port(1).is_some(),
            AddressSpace::Memory | AddressSpace::PciConfig => true,
            AddressSpace::Other(_) => false,
        };
        Ok(usable.then_some(ResetReg { register, value }))
    }
}
