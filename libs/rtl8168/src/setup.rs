//! Bringing the chip up and the small register conversations after that:
//! identify the revision, reset, the MAC address, link state, interrupt
//! causes, shutdown, and a register dump for diagnosis.

use crate::regs::{cfg9346, cmd, int, phy_status, tx_config, Regs};
use crate::regs::{CFG9346, CHIP_CMD, DUMP_BYTES, IDR0, IDR4, INTR_MASK, INTR_STATUS};
use crate::regs::{PHY_STATUS, TX_CONFIG};

/// Polls of `ChipCmd.RESET` before giving up.
const POLLS: u32 = 1000;

/// The only revision driven: Linux's "RTL8168h/8111h".
pub const XID_8168H: u16 = 0x541;

/// Why bring-up failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupError {
    /// Every register reads as ones: the function is gone (or the BAR is not
    /// the chip's).
    DeviceGone,
    /// A revision of the family this driver does not drive, by XID. The
    /// binary reports it and parks rather than restarting.
    Unsupported { xid: u16 },
    /// `ChipCmd.RESET` never cleared.
    ResetTimeout,
    /// The station address registers hold nothing usable (all zero or a group
    /// address).
    NoMac,
    /// A `PHYAR` access never completed.
    PhyTimeout,
    /// No PHY answers on the MII (both ID registers read as ones or zeros).
    NoPhy,
}

/// The revision field of `TxConfig`: the bits the chip scatters over two
/// groups, gathered.
pub fn xid(tx_config: u32) -> u16 {
    ((tx_config >> tx_config::XID_SHIFT) & tx_config::XID_MASK) as u16
}

/// Check that the function is a chip this driver drives; the XID on success.
pub fn identify(regs: &impl Regs) -> Result<u16, SetupError> {
    let config = regs.read32(TX_CONFIG);
    if config == u32::MAX {
        return Err(SetupError::DeviceGone);
    }
    match xid(config) {
        XID_8168H => Ok(XID_8168H),
        xid => Err(SetupError::Unsupported { xid }),
    }
}

/// Mask every interrupt and acknowledge every pending cause.
fn quiet(regs: &mut impl Regs) {
    regs.write16(INTR_MASK, 0);
    regs.write16(INTR_STATUS, u16::MAX);
}

/// Soft-reset the chip and leave it quiet: every interrupt masked and
/// acknowledged, receiver and transmitter off. `wait` is called between polls
/// (the driver naps one tick).
pub fn reset(regs: &mut impl Regs, mut wait: impl FnMut()) -> Result<(), SetupError> {
    quiet(regs);
    regs.write8(CHIP_CMD, cmd::RESET);
    let mut polls = 0;
    while regs.read8(CHIP_CMD) & cmd::RESET != 0 {
        polls += 1;
        if polls >= POLLS {
            return Err(SetupError::ResetTimeout);
        }
        wait();
    }
    // The reset re-arms nothing, but the EEPROM/eFuse load that follows it can
    // leave causes pending.
    quiet(regs);
    Ok(())
}

/// A MAC that can be a station address: not all zero, not a group address.
fn usable(mac: [u8; 6]) -> Option<[u8; 6]> {
    (mac != [0; 6] && mac[0] & 1 == 0).then_some(mac)
}

/// The station address the chip loaded at power-on.
pub fn mac(regs: &impl Regs) -> Result<[u8; 6], SetupError> {
    let low = regs.read32(IDR0).to_le_bytes();
    let high = regs.read32(IDR4).to_le_bytes();
    usable([low[0], low[1], low[2], low[3], high[0], high[1]]).ok_or(SetupError::NoMac)
}

/// Writes to the config registers (`Config1..5`, and the receive and transmit
/// configuration on some revisions) are accepted only while unlocked.
pub fn unlock_config(regs: &mut impl Regs) {
    regs.write8(CFG9346, cfg9346::UNLOCK);
}

pub fn lock_config(regs: &mut impl Regs) {
    regs.write8(CFG9346, cfg9346::LOCK);
}

/// What `PHYstatus` says about the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub up: bool,
    /// Megabits per second when the link is up; 0 if the speed bits say none.
    pub mbps: u16,
    pub full_duplex: bool,
}

/// Decode `PHYstatus`; `None` when it reads all ones (a gone device: no real
/// chip reports 10, 100 and 1000 at once).
pub fn link(regs: &impl Regs) -> Option<Link> {
    let status = regs.read8(PHY_STATUS);
    if status == u8::MAX {
        return None;
    }
    let up = status & phy_status::LINK != 0;
    let mbps = if !up {
        0
    } else if status & phy_status::SPEED_1000 != 0 {
        1000
    } else if status & phy_status::SPEED_100 != 0 {
        100
    } else if status & phy_status::SPEED_10 != 0 {
        10
    } else {
        0
    };
    Some(Link {
        up,
        mbps,
        full_duplex: status & phy_status::FULL_DUPLEX != 0,
    })
}

/// Unmask the causes the driver handles.
pub fn enable_interrupts(regs: &mut impl Regs) {
    regs.write16(INTR_MASK, int::WANTED);
}

/// Read the pending causes and acknowledge exactly those. Must happen before
/// the kernel is told to unmask the line. `u16::MAX` is a gone device, which
/// the caller treats as fatal.
pub fn take_causes(regs: &mut impl Regs) -> u16 {
    let causes = regs.read16(INTR_STATUS);
    if causes != 0 && causes != u16::MAX {
        regs.write16(INTR_STATUS, causes);
    }
    causes
}

/// Stop the chip's DMA for good: receiver and transmitter off, interrupts
/// masked, soft reset. After this nothing the chip does touches memory.
pub fn shutdown(regs: &mut impl Regs, wait: impl FnMut()) -> Result<(), SetupError> {
    regs.write8(CHIP_CMD, 0);
    reset(regs, wait)
}

/// Read the first [`DUMP_BYTES`] of register space, a dword at a time like
/// `ethtool -d` does, for `tools/net/rtl8168/` to decode and to compare with
/// Linux's dump of the same chip.
pub fn dump(regs: &impl Regs, out: &mut [u8; DUMP_BYTES]) {
    let (words, _) = out.as_chunks_mut::<4>();
    for (index, word) in words.iter_mut().enumerate() {
        *word = regs.read32(index as u32 * 4).to_le_bytes();
    }
}
