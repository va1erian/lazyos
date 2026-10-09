//! The MII conversation with the integrated PHY, over `PHYAR`.
//!
//! A read writes the register number with the flag clear and waits for the
//! chip to set the flag with the data; a write sets the flag with the data and
//! waits for the chip to clear it. Standard MII registers only (IEEE 802.3
//! clause 22): `phy_541.rs` is where anything revision-specific goes.

use crate::regs::{phyar, Regs, PHYAR};
use crate::setup::SetupError;

/// Polls of `PHYAR` before giving up.
const POLLS: u32 = 100;

/// Standard MII registers.
pub mod mii {
    pub const BMCR: u8 = 0;
    pub const BMSR: u8 = 1;
    pub const PHYID1: u8 = 2;
    pub const PHYID2: u8 = 3;
    pub const ANAR: u8 = 4;
    pub const ANLPAR: u8 = 5;
    /// 1000BASE-T control.
    pub const GBCR: u8 = 9;
    /// 1000BASE-T status.
    pub const GBSR: u8 = 10;
    /// Realtek's page select; page 0 holds the standard registers.
    pub const PAGE: u8 = 31;

    /// `BMCR` bits.
    pub const BMCR_RESET: u16 = 1 << 15;
    pub const BMCR_AUTONEG: u16 = 1 << 12;
    pub const BMCR_RESTART: u16 = 1 << 9;
    /// `ANAR`: the 802.3 selector, then 10/100 half and full duplex.
    pub const ANAR_ALL_10_100: u16 = 0x0001 | 0x01E0;
    /// `GBCR`: advertise 1000BASE-T full duplex.
    pub const GBCR_1000_FULL: u16 = 1 << 9;
}

/// Read PHY register `reg`.
pub fn read(regs: &mut impl Regs, reg: u8, mut wait: impl FnMut()) -> Result<u16, SetupError> {
    let reg = u32::from(reg) & phyar::REG_MASK;
    regs.write32(PHYAR, reg << phyar::REG_SHIFT);
    for _ in 0..POLLS {
        let value = regs.read32(PHYAR);
        if value & phyar::FLAG != 0 {
            return Ok((value & phyar::DATA_MASK) as u16);
        }
        wait();
    }
    Err(SetupError::PhyTimeout)
}

/// Write PHY register `reg`.
pub fn write(
    regs: &mut impl Regs,
    reg: u8,
    value: u16,
    mut wait: impl FnMut(),
) -> Result<(), SetupError> {
    let reg = u32::from(reg) & phyar::REG_MASK;
    regs.write32(
        PHYAR,
        phyar::FLAG | reg << phyar::REG_SHIFT | u32::from(value),
    );
    for _ in 0..POLLS {
        if regs.read32(PHYAR) & phyar::FLAG == 0 {
            return Ok(());
        }
        wait();
    }
    Err(SetupError::PhyTimeout)
}

/// Set `mask` bits of register `reg` to `value`'s, leaving the rest.
pub fn update(
    regs: &mut impl Regs,
    reg: u8,
    mask: u16,
    value: u16,
    mut wait: impl FnMut(),
) -> Result<(), SetupError> {
    let old = read(regs, reg, &mut wait)?;
    write(regs, reg, (old & !mask) | (value & mask), wait)
}

/// The PHY's two ID registers; `NoPhy` if nothing answers.
pub fn id(regs: &mut impl Regs, mut wait: impl FnMut()) -> Result<u32, SetupError> {
    let high = read(regs, mii::PHYID1, &mut wait)?;
    let low = read(regs, mii::PHYID2, &mut wait)?;
    match (high, low) {
        (0xFFFF, 0xFFFF) | (0, 0) => Err(SetupError::NoPhy),
        _ => Ok(u32::from(high) << 16 | u32::from(low)),
    }
}

/// Restart autonegotiation advertising 10/100/1000 full and half duplex (the
/// 1000 half-duplex bit stays clear, as the standard recommends), no flow
/// control. Page 0 is selected first so the standard registers are the ones
/// written. Returns the PHY ID it found.
pub fn start_autoneg(regs: &mut impl Regs, mut wait: impl FnMut()) -> Result<u32, SetupError> {
    write(regs, mii::PAGE, 0, &mut wait)?;
    let id = id(regs, &mut wait)?;
    write(regs, mii::ANAR, mii::ANAR_ALL_10_100, &mut wait)?;
    update(
        regs,
        mii::GBCR,
        mii::GBCR_1000_FULL | 1 << 8,
        mii::GBCR_1000_FULL,
        &mut wait,
    )?;
    write(
        regs,
        mii::BMCR,
        mii::BMCR_AUTONEG | mii::BMCR_RESTART,
        &mut wait,
    )?;
    Ok(id)
}
