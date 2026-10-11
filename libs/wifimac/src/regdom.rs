//! Which channels a regulatory domain lets the radio use, and where it may
//! transmit first (an active probe) rather than only listen.
//!
//! A coarse table, not a regulatory database: it errs towards listening.
//! The world domain (`00`, what applies before `SetCountry`) probes only on
//! 2.4 GHz channels 1-11 and listens everywhere else it may tune. A country
//! in the FCC group stops at channel 11; any other country code gets
//! channels 12 and 13 too. On 5 GHz, UNII-1 (36-48) and UNII-3 (149-165) are
//! active for a named country, the DFS channels (52-64, 100-144) are always
//! passive (no radar detection here), and nothing else is used. 6 GHz is not
//! used at all in v1.

use alloc::vec::Vec;

use crate::radio::{Band, Channel};

/// How a channel may be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    /// Probe requests may be sent.
    Active,
    /// Listen only until an access point is heard.
    Passive,
}

/// Countries that follow the FCC's 2.4 GHz plan (channels 1-11).
const FCC: &[[u8; 2]] = &[
    *b"US", *b"CA", *b"MX", *b"PR", *b"TW", *b"CO", *b"DO", *b"GT", *b"PA", *b"GU", *b"AS", *b"VI",
    *b"UM", *b"MP",
];

const UNII1: [u8; 4] = [36, 40, 44, 48];
const DFS: [u8; 16] = [
    52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144,
];
const UNII3: [u8; 5] = [149, 153, 157, 161, 165];

/// A regulatory domain: `00` (world) or an ISO 3166 alpha-2 code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Domain {
    code: [u8; 2],
}

impl Domain {
    /// The world domain.
    pub const WORLD: Domain = Domain { code: *b"00" };

    /// Parse `00` or two upper-case ASCII letters; anything else is `None`
    /// (`SetCountry` answers `EINVAL`).
    pub fn parse(text: &str) -> Option<Domain> {
        let code: [u8; 2] = text.as_bytes().try_into().ok()?;
        let world = code == *b"00";
        if world || code.iter().all(u8::is_ascii_uppercase) {
            Some(Domain { code })
        } else {
            None
        }
    }

    pub fn code(&self) -> [u8; 2] {
        self.code
    }

    fn world(&self) -> bool {
        self.code == *b"00"
    }

    /// How `band`/`number` may be used, `None` when not at all.
    pub fn allows(&self, band: Band, number: u8) -> Option<Use> {
        let named = if self.world() {
            Use::Passive
        } else {
            Use::Active
        };
        match band {
            Band::Ghz2 => match number {
                1..=11 => Some(Use::Active),
                12 | 13 if self.world() => Some(Use::Passive),
                12 | 13 if FCC.contains(&self.code) => None,
                12 | 13 => Some(Use::Active),
                _ => None,
            },
            Band::Ghz5 if UNII1.contains(&number) || UNII3.contains(&number) => Some(named),
            Band::Ghz5 if DFS.contains(&number) => Some(Use::Passive),
            _ => None,
        }
    }

    /// Every channel of `band` the domain allows, in order.
    pub fn channels(&self, band: Band) -> Vec<Channel> {
        let candidates: Vec<u8> = match band {
            Band::Ghz2 => (1..=13).collect(),
            Band::Ghz5 => UNII1
                .iter()
                .chain(DFS.iter())
                .chain(UNII3.iter())
                .copied()
                .collect(),
            Band::Ghz6 => Vec::new(),
        };
        candidates
            .into_iter()
            .filter(|&n| self.allows(band, n).is_some())
            .map(|n| Channel::new(band, n))
            .collect()
    }
}
