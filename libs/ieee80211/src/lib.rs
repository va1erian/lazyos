//! IEEE 802.11 management frames, information elements and a scan table
//! (`docs/wifi-prerequisites-plan.md` WP3, section 3.4).
//!
//! Everything here reads bytes that came through the air from an access point
//! anyone can run, so a hostile beacon is the normal case: every length is
//! checked before use, every loop is bounded by its input, allocation is
//! bounded by the input length (or by a stated cap), and malformed input is an
//! [`Error`] or a counted, dropped element, never a panic or an out-of-bounds
//! read. The library has no `unsafe`.
//!
//! * [`ie`] walks information elements and extracts the ones a station reads
//!   ([`Elements`]).
//! * [`rsn`] parses and builds the RSN element ([`Rsn`]): ciphers, AKMs,
//!   capabilities, PMKIDs.
//! * [`frame`] parses management frames ([`Mgmt`]); [`build`] builds them.
//! * [`bss`] summarises a beacon or probe response ([`Bss`]); [`table`] keeps
//!   the scan results with ageing and a capacity bound ([`ScanTable`]).
//!
//! Frames start at the 802.11 MAC header and have **no FCS** (the chip strips
//! it). Data frames are out of scope: the data path arrives as Ethernet from
//! the chip. Time is a caller-supplied millisecond counter; the library keeps
//! no clock.
//!
//! Written from IEEE Std 802.11-2020 (clauses 9.2-9.4). No code or structure
//! was taken from any other implementation; see `THIRD_PARTY.md`.
//!
//! # Behaviour on malformed elements
//!
//! * A truncated element at the end of a buffer ends the walk; the elements
//!   before it are kept and [`Elements::truncated`] is set.
//! * When a singleton element (SSID, rates, DS parameter set, country, HT/VHT/
//!   HE capabilities, RSN) appears more than once, the **first** wins and
//!   [`Elements::duplicates`] counts the rest.
//! * A known element with an impossible length (a DS parameter set that is not
//!   one octet, a country element under three octets) is ignored and counted
//!   in [`Elements::malformed`]. An RSN element that fails [`Rsn::parse_body`]
//!   makes the BSS [`Security::InvalidRsn`], which no connection logic selects.
//! * An SSID longer than 32 octets is the one hard error ([`Error::BadSsid`]):
//!   the whole frame is refused, since such a BSS cannot be named.
//! * Vendor elements are recognised and kept up to [`ie::MAX_VENDOR`]; the
//!   rest are counted in [`Elements::vendor_dropped`].

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod bss;
pub mod build;
pub mod frame;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod ie;
pub mod rsn;
pub mod table;
#[cfg(test)]
mod tests;

pub use bss::{Bss, Security};
pub use frame::{Body, Header, Mgmt};
pub use ie::{Element, Elements};
pub use rsn::{Akm, Cipher, Rsn, RsnCaps};
pub use table::{ScanTable, Update};

/// Why a frame or element was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Fewer bytes than the header or fixed fields need.
    Short,
    /// The frame control protocol version is not 0.
    BadVersion,
    /// A control or data frame; only management frames are parsed.
    NotManagement,
    /// The Protected Frame bit is set: the body is encrypted (protected
    /// management frames are not supported, so they are not read).
    Protected,
    /// A fragment of a fragmented management frame (the library does not
    /// reassemble).
    Fragmented,
    /// An SSID element longer than 32 octets.
    BadSsid,
    /// A value does not fit its one-octet element length when building.
    IeTooLong,
    /// The RSN element is malformed.
    Rsn(rsn::RsnError),
}

impl Error {
    /// A short explanation for logs.
    pub const fn message(self) -> &'static str {
        match self {
            Error::Short => "the frame is too short",
            Error::BadVersion => "the frame control version is not 0",
            Error::NotManagement => "not a management frame",
            Error::Protected => "the management frame is protected",
            Error::Fragmented => "a fragmented management frame",
            Error::BadSsid => "the SSID is longer than 32 octets",
            Error::IeTooLong => "an information element body is over 255 octets",
            Error::Rsn(_) => "the RSN element is malformed",
        }
    }
}

impl From<rsn::RsnError> for Error {
    fn from(error: rsn::RsnError) -> Error {
        Error::Rsn(error)
    }
}

/// A MAC address.
pub type Mac = [u8; 6];
/// The broadcast address (and the wildcard BSSID of a probe request).
pub const BROADCAST: Mac = [0xFF; 6];

/// The group bit of an address: set for multicast and broadcast.
pub fn is_group(addr: &Mac) -> bool {
    addr[0] & 1 != 0
}

/// Capability information bits (9.4.1.4).
pub mod cap {
    /// The sender is an access point (infrastructure BSS).
    pub const ESS: u16 = 1 << 0;
    /// The sender is in an IBSS (ad hoc).
    pub const IBSS: u16 = 1 << 1;
    /// Data confidentiality is required (WEP, or RSN with a cipher).
    pub const PRIVACY: u16 = 1 << 4;
}
