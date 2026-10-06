//! Bringing an 8254x up and the small register conversations after that:
//! reset, the MAC address, link state and interrupt causes.

use crate::regs::{ctrl, eerd, int, status, Regs, CTRL, EERD, ICR, IMC, IMS, MTA, MTA_WORDS};
use crate::regs::{RAH0, RAH_AV, RAL0, STATUS};

/// Polls of `CTRL.RST` (and of an EEPROM read) before giving up.
const POLLS: u32 = 1000;

/// Why bring-up failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupError {
    /// `CTRL.RST` never cleared.
    ResetTimeout,
    /// Neither the receive-address registers nor the EEPROM hold a usable
    /// (unicast, non-zero) address.
    NoMac,
}

/// Reset the controller and leave it quiet: every interrupt masked, the
/// multicast table clear, link forced up with auto-speed. `wait` is called
/// between polls (the driver naps one tick). The receive and transmit units
/// stay off until [`crate::Rings::new`] programs them.
pub fn reset(regs: &mut impl Regs, mut wait: impl FnMut()) -> Result<(), SetupError> {
    regs.write(IMC, int::ALL);
    regs.write(CTRL, regs.read(CTRL) | ctrl::RST);
    // The manual asks for a short wait before the register reads back sane.
    wait();
    let mut polls = 0;
    while regs.read(CTRL) & ctrl::RST != 0 {
        polls += 1;
        if polls >= POLLS {
            return Err(SetupError::ResetTimeout);
        }
        wait();
    }
    // A reset re-enables nothing, but the mask is cleared again in case the
    // EEPROM load set any cause.
    regs.write(IMC, int::ALL);
    let _ = regs.read(ICR);
    let control = (regs.read(CTRL) | ctrl::SLU | ctrl::ASDE) & !ctrl::PHY_RST;
    regs.write(CTRL, control);
    for word in 0..MTA_WORDS {
        regs.write(MTA + word * 4, 0);
    }
    Ok(())
}

/// A MAC that can be a station address: not all zero, not a group address.
fn usable(mac: [u8; 6]) -> Option<[u8; 6]> {
    (mac != [0; 6] && mac[0] & 1 == 0).then_some(mac)
}

/// The station address: receive address 0 when the EEPROM load marked it
/// valid, else the first three EEPROM words.
pub fn mac(regs: &mut impl Regs, mut wait: impl FnMut()) -> Result<[u8; 6], SetupError> {
    let high = regs.read(RAH0);
    if high & RAH_AV != 0 {
        let low = regs.read(RAL0).to_le_bytes();
        let high = high.to_le_bytes();
        if let Some(mac) = usable([low[0], low[1], low[2], low[3], high[0], high[1]]) {
            return Ok(mac);
        }
    }
    let mut mac = [0u8; 6];
    for word in 0..3u32 {
        regs.write(EERD, eerd::START | word << eerd::ADDR_SHIFT);
        let mut polls = 0;
        let value = loop {
            let value = regs.read(EERD);
            if value & eerd::DONE != 0 {
                break value;
            }
            polls += 1;
            if polls >= POLLS {
                return Err(SetupError::NoMac);
            }
            wait();
        };
        let data = ((value >> eerd::DATA_SHIFT) as u16).to_le_bytes();
        mac[word as usize * 2] = data[0];
        mac[word as usize * 2 + 1] = data[1];
    }
    usable(mac).ok_or(SetupError::NoMac)
}

/// Whether the link is up.
pub fn link_up(regs: &impl Regs) -> bool {
    regs.read(STATUS) & status::LU != 0
}

/// Unmask the causes the driver handles.
pub fn enable_interrupts(regs: &mut impl Regs) {
    regs.write(IMS, int::WANTED);
}

/// Read (and so clear) the pending causes. Reading `ICR` deasserts the
/// interrupt, which must happen before the kernel is told to unmask the line.
pub fn take_causes(regs: &mut impl Regs) -> u32 {
    regs.read(ICR)
}
