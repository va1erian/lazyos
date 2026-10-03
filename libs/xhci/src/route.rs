//! Where a device sits in the USB tree, as the slot context needs it
//! (xHCI 4.3.3, 6.2.2; USB 3.2 10.16.2.9 for the route string).
//!
//! A device on a root port has route string 0. Each hub tier below it adds
//! the hub port it hangs off as one nibble, tier 1 in bits 0..=3, so a
//! route string names at most five hubs and ports 1..=15. A low- or
//! full-speed device behind a high-speed hub is reached through that hub's
//! transaction translator; one behind a full-speed hub that itself sits
//! behind a high-speed hub uses the same translator as its hub.

use core::fmt;

use crate::context::{HubSlot, SlotContext, Tt};
use crate::regs::Speed;
use crate::Error;

/// Hub tiers a route string can describe.
pub const MAX_TIERS: u8 = 5;
/// The highest hub port a route string nibble can name.
pub const MAX_HUB_PORT: u8 = 15;

/// A device's place in the tree and the speed it runs at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    /// The root hub port everything above it hangs off (1-based).
    pub root_port: u8,
    pub route: u32,
    /// Hubs between the root port and this device.
    pub depth: u8,
    pub speed: Speed,
    pub tt: Option<Tt>,
}

impl Location {
    /// A device plugged straight into root port `root_port`.
    pub fn root(root_port: u8, speed: Speed) -> Location {
        Location {
            root_port,
            route: 0,
            depth: 0,
            speed,
            tt: None,
        }
    }

    /// A device of `speed` on `port` of the hub at `self`, which has slot
    /// `hub_slot` and (if high speed) one TT per port when `multi_tt`.
    /// Refused: too deep, a port a route string cannot name, or a speed
    /// the hub cannot carry (high speed below a full-speed hub, anything but
    /// SuperSpeed below a SuperSpeed hub).
    pub fn child(
        &self,
        hub_slot: u8,
        multi_tt: bool,
        port: u8,
        speed: Speed,
    ) -> Result<Location, Error> {
        if self.depth >= MAX_TIERS || !(1..=MAX_HUB_PORT).contains(&port) || hub_slot == 0 {
            return Err(Error::BadArgument);
        }
        let usb3 = |s: Speed| matches!(s, Speed::Super | Speed::SuperPlus);
        let slow = |s: Speed| matches!(s, Speed::Low | Speed::Full);
        if usb3(self.speed) != usb3(speed) || (slow(self.speed) && !slow(speed)) {
            return Err(Error::BadArgument);
        }
        let tt = match (slow(speed), self.speed) {
            (true, Speed::High) => Some(Tt {
                hub_slot,
                port,
                multi: multi_tt,
            }),
            (true, _) => self.tt,
            (false, _) => None,
        };
        Ok(Location {
            root_port: self.root_port,
            route: self.route | u32::from(port) << (4 * self.depth),
            depth: self.depth + 1,
            speed,
            tt,
        })
    }

    /// The slot context for this location with endpoints up to `entries`,
    /// declaring a hub when `hub` is set.
    pub fn slot_context(&self, entries: u8, hub: Option<HubSlot>) -> SlotContext {
        SlotContext {
            route: self.route,
            speed: self.speed,
            entries,
            root_port: self.root_port,
            tt: self.tt,
            hub,
        }
    }

    /// The hub port of tier `tier` (1-based) on the way down, if that deep.
    pub fn hub_port(&self, tier: u8) -> Option<u8> {
        (tier >= 1 && tier <= self.depth).then(|| ((self.route >> (4 * (tier - 1))) & 0xF) as u8)
    }

    /// Whether `self` is `other` or hangs (at any depth) below it.
    pub fn is_within(&self, other: &Location) -> bool {
        let mask = (1u32 << (4 * u32::from(other.depth))) - 1;
        self.root_port == other.root_port
            && self.depth >= other.depth
            && self.route & mask == other.route
    }
}

/// `root.port.port...`, the way the serial markers name a device.
impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.root_port)?;
        for tier in 1..=self.depth {
            write!(f, ".{}", self.hub_port(tier).unwrap_or(0))?;
        }
        Ok(())
    }
}
