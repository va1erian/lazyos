//! The Wi-Fi station state machine of `wlanmd`
//! (docs/wifi-prerequisites-plan.md WP4, section 3.5; docs/architecture/wifi.md).
//!
//! One [`Station`] runs one radio. It is pure: [`Station::input`] takes an
//! [`Event`] (something the radio reported, or the answer to an earlier
//! request) and the caller's millisecond clock, and returns the [`Action`]s to
//! perform, in order. It has no clock and no I/O, so every path (timeouts and
//! retransmissions included) is tested on the host.
//!
//! ```text
//!  Idle ─▶ Scanning ─▶ (PMK from keyd) ─▶ Authenticating ─▶ Associating
//!     ▲                                                        │
//!     │      open: SetState(Authorized) ◀──────────────────────┤
//!     │      WPA2: Handshake (eapol) ─▶ SetKey… ─▶ Authorized ─┘
//!     │                                       │
//!  Failed ◀─ timeouts, rejections, deauth, beacon loss ── Connected
//!     └── back-off, then again (never after a wrong passphrase)
//! ```
//!
//! * **Scanning** asks the radio for a scan and parses the raw beacons and
//!   probe responses it returns with `ieee80211` into a bounded
//!   [`ieee80211::ScanTable`]; hostile frames are dropped by that library.
//! * **Selection** ([`select`]): a known SSID whose security is the profile's,
//!   highest priority then strongest signal. Only Open and WPA2-PSK with
//!   CCMP-128 (AKM 2 or 6, no PMF required) are joinable; the rest are listed
//!   as [`Security::Unsupported`].
//! * **Joining**: Open System authentication, then an association request
//!   whose RSN element states exactly one cipher and AKM, the one `eapol`
//!   will check message 2 and 3 against. Each is retransmitted
//!   [`JOIN_TRIES`] times, [`JOIN_TIMEOUT_MS`] apart.
//! * **Handshake**: EAPOL frames go to an `eapol::Supplicant`, its actions
//!   become `TxMgmt(Eapol)`, `SetKey` and `SetState(Authorized)` in the order
//!   it gives them. The supplicant has no timers; the station gives the 4-way
//!   handshake [`HANDSHAKE_TIMEOUT_MS`]. A handshake that sent message 2 and
//!   never got a verified message 3 (timeout, or the AP deauthenticating us)
//!   is [`FailReason::BadPassphrase`]: the AP drops a message 2 whose MIC
//!   fails, which is all a wrong passphrase looks like from here.
//! * **Connected**: group rekeys and PTK rekeys keep flowing through the
//!   same supplicant. No timer runs here, so a forged message 1 cannot make
//!   the station drop its own link.
//! * **Failures** go to [`ConnState::Failed`] with a [`FailReason`] and a
//!   retry after a back-off of [`BACKOFF_MIN_MS`] doubling to
//!   [`BACKOFF_MAX_MS`]. A wrong passphrase blocks that profile until the
//!   user connects to it again, so it is never retried on its own (no retry
//!   storm against an AP, no account lockout).
//! * [`Station::disconnect`] leaves and suspends auto-connect until the next
//!   [`Station::connect`].
//!
//! The caller (`wlanmd`) runs the actions against `os.lazy.net.wifi.hw.v1`,
//! stops a batch at the first radio call that fails and reports it as
//! [`Event::HwError`], fetches PMKs from `keyd` for [`Action::FetchPmk`], and
//! publishes [`Station::status`] whenever its `changes` counter moves.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

mod frames;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
mod join;
mod machine;
mod networks;
pub mod policy;
pub mod profile;
pub mod select;
pub mod ssid;
#[cfg(test)]
mod tests;
mod types;

pub use eapol::Key;
pub use machine::Station;
pub use networks::{Network, Viewer};
pub use types::*;

/// Wait for an authentication or association response this long.
pub const JOIN_TIMEOUT_MS: u64 = 500;
/// Send an authentication or association request at most this many times.
pub const JOIN_TRIES: u8 = 4;
/// The whole 4-way handshake must finish this long after association. The
/// standard's authenticator retries message 1 and 3 for about a second; the
/// margin is for a PBKDF2-free but slow (emulated) guest.
pub const HANDSHAKE_TIMEOUT_MS: u64 = 5_000;
/// A scan that has not reported done by then is aborted and its results
/// read as they are.
pub const SCAN_TIMEOUT_MS: u64 = 20_000;
/// Scan entries not heard for this long are forgotten.
pub const SCAN_MAX_AGE_MS: u64 = 120_000;
/// A BSS heard this recently is joined without scanning first.
pub const FRESH_MS: u64 = 30_000;
/// While idle with networks to auto-connect to and none in range, scan again
/// this often.
pub const AUTO_SCAN_MS: u64 = 60_000;
/// First retry delay after a failure; it doubles per failure up to the max.
pub const BACKOFF_MIN_MS: u64 = 1_000;
pub const BACKOFF_MAX_MS: u64 = 60_000;
/// BSSes the scan table keeps.
pub const SCAN_CAPACITY: usize = 128;
/// SSIDs one scan request may probe for (`os.lazy.net.wifi.v1` `Scan`).
pub const MAX_SCAN_SSIDS: usize = 4;
/// Networks [`Station::networks`] lists at most (`Networks`).
pub const MAX_NETWORKS: usize = 128;

/// IEEE 802.11 reason codes the station sends (Table 9-49).
pub mod reason {
    pub const UNSPECIFIED: u16 = 1;
    /// The station is leaving (a user disconnect).
    pub const LEAVING: u16 = 3;
    /// Inactivity: the AP went silent.
    pub const INACTIVITY: u16 = 4;
    /// The 4-way handshake timed out.
    pub const HANDSHAKE_TIMEOUT: u16 = 15;
}
