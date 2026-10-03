//! The hub class (USB 2.0 11.23-11.24, USB 3.2 10.15-10.16): the hub
//! descriptor, a port's status and change bits, and the status-change
//! bitmap the hub's interrupt endpoint reports.
//!
//! A hub is a device like any other and may lie: the descriptor is checked
//! before use, the port count is bounded by what a route string can name,
//! and status words are decoded only from the four bytes they occupy.

use crate::Error;

/// Hub descriptor types: USB 2 and SuperSpeed.
pub const HUB_DESCRIPTOR: u8 = 0x29;
pub const SS_HUB_DESCRIPTOR: u8 = 0x2A;
/// The most ports driven per hub: a route string nibble names 1..=15.
pub const MAX_PORTS: u8 = 15;
/// Bytes of a status-change bitmap for [`MAX_PORTS`] ports (bit 0 is the hub).
pub const BITMAP_BYTES: usize = 2;

/// Port feature selectors (USB 2.0 Table 11-17, USB 3.2 Table 10-10).
pub mod feature {
    pub const PORT_RESET: u16 = 4;
    pub const PORT_POWER: u16 = 8;
    pub const C_PORT_CONNECTION: u16 = 16;
    pub const C_PORT_ENABLE: u16 = 17;
    pub const C_PORT_SUSPEND: u16 = 18;
    pub const C_PORT_OVER_CURRENT: u16 = 19;
    pub const C_PORT_RESET: u16 = 20;
    pub const C_PORT_LINK_STATE: u16 = 25;
    pub const C_PORT_CONFIG_ERROR: u16 = 26;
    pub const BH_PORT_RESET: u16 = 28;
    pub const C_BH_PORT_RESET: u16 = 29;
}

/// What the driver needs from a hub descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HubDescriptor {
    /// `bNbrPorts` as the hub reported it (the driver uses at most
    /// [`MAX_PORTS`]).
    pub ports: u8,
    pub characteristics: u16,
    /// Time from power-on to power-good, in milliseconds.
    pub power_on_ms: u16,
    /// TT Think Time (`wHubCharacteristics` bits 5..=6, USB 2 hubs).
    pub think_time: u8,
}

/// Parse a USB 2 (`superspeed == false`) or SuperSpeed hub descriptor.
pub fn parse_hub(bytes: &[u8], superspeed: bool) -> Result<HubDescriptor, Error> {
    let (kind, min) = if superspeed {
        (SS_HUB_DESCRIPTOR, 12)
    } else {
        (HUB_DESCRIPTOR, 7)
    };
    if bytes.len() < min {
        return Err(Error::Short);
    }
    if bytes[1] != kind {
        return Err(Error::WrongType);
    }
    if usize::from(bytes[0]) < min || usize::from(bytes[0]) > bytes.len() || bytes[2] == 0 {
        return Err(Error::BadLength);
    }
    let characteristics = u16::from_le_bytes([bytes[3], bytes[4]]);
    Ok(HubDescriptor {
        ports: bytes[2],
        characteristics,
        power_on_ms: u16::from(bytes[5]) * 2,
        think_time: if superspeed {
            0
        } else {
            ((characteristics >> 5) & 0x3) as u8
        },
    })
}

/// The speed a hub port reports for its device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortSpeed {
    Low,
    Full,
    High,
    Super,
}

/// `wPortStatus` and `wPortChange` of one port (GET_STATUS, 4 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortStatus {
    pub status: u16,
    pub change: u16,
    /// A SuperSpeed hub's port (the bits differ, USB 3.2 10.16.2.6).
    pub superspeed: bool,
}

impl PortStatus {
    pub fn decode(bytes: &[u8], superspeed: bool) -> Result<PortStatus, Error> {
        let bytes = bytes.get(..4).ok_or(Error::Short)?;
        Ok(PortStatus {
            status: u16::from_le_bytes([bytes[0], bytes[1]]),
            change: u16::from_le_bytes([bytes[2], bytes[3]]),
            superspeed,
        })
    }

    pub fn connected(&self) -> bool {
        self.status & 1 != 0
    }

    pub fn enabled(&self) -> bool {
        self.status & (1 << 1) != 0
    }

    pub fn resetting(&self) -> bool {
        self.status & (1 << 4) != 0
    }

    pub fn powered(&self) -> bool {
        let bit = if self.superspeed { 9 } else { 8 };
        self.status & (1 << bit) != 0
    }

    /// A SuperSpeed port's link state (bits 5..=8), 0 on USB 2 hubs.
    pub fn link_state(&self) -> u8 {
        if self.superspeed {
            ((self.status >> 5) & 0xF) as u8
        } else {
            0
        }
    }

    /// The attached device's speed: USB 2 hubs report low and high speed
    /// bits (neither is full speed); SuperSpeed hub ports carry only
    /// SuperSpeed devices.
    pub fn speed(&self) -> PortSpeed {
        match (
            self.superspeed,
            self.status & (1 << 9) != 0,
            self.status & (1 << 10) != 0,
        ) {
            (true, _, _) => PortSpeed::Super,
            (false, true, _) => PortSpeed::Low,
            (false, false, true) => PortSpeed::High,
            (false, false, false) => PortSpeed::Full,
        }
    }

    pub fn connect_changed(&self) -> bool {
        self.change & 1 != 0
    }

    pub fn reset_changed(&self) -> bool {
        // C_PORT_RESET, or on SuperSpeed hubs also C_BH_PORT_RESET.
        self.change & (1 << 4) != 0 || (self.superspeed && self.change & (1 << 5) != 0)
    }

    /// The CLEAR_FEATURE selectors that acknowledge every change bit set.
    pub fn change_features(&self) -> impl Iterator<Item = u16> + '_ {
        let table: &[(u16, u16)] = if self.superspeed {
            &[
                (1, feature::C_PORT_CONNECTION),
                (1 << 3, feature::C_PORT_OVER_CURRENT),
                (1 << 4, feature::C_PORT_RESET),
                (1 << 5, feature::C_BH_PORT_RESET),
                (1 << 6, feature::C_PORT_LINK_STATE),
                (1 << 7, feature::C_PORT_CONFIG_ERROR),
            ]
        } else {
            &[
                (1, feature::C_PORT_CONNECTION),
                (1 << 1, feature::C_PORT_ENABLE),
                (1 << 2, feature::C_PORT_SUSPEND),
                (1 << 3, feature::C_PORT_OVER_CURRENT),
                (1 << 4, feature::C_PORT_RESET),
            ]
        };
        table
            .iter()
            .filter(|(bit, _)| self.change & bit != 0)
            .map(|&(_, selector)| selector)
    }
}

/// The ports (1..=`ports`, at most [`MAX_PORTS`]) a status-change bitmap
/// flags; bit 0 (the hub itself) is not a port.
pub fn changed_ports(bitmap: &[u8], ports: u8) -> impl Iterator<Item = u8> + '_ {
    (1..=ports.min(MAX_PORTS)).filter(move |&port| {
        bitmap
            .get(usize::from(port / 8))
            .is_some_and(|byte| byte & (1 << (port % 8)) != 0)
    })
}
