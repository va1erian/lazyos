//! The soft-MAC both Wi-Fi radio programs share (docs/wifi-rtw88-plan.md
//! section 3): `wifid` over the RTL8822CE and `wifisim` over a simulated air.
//!
//! A [`SoftMac`] sits on a [`Radio`] and does everything the hardware does
//! not, as pure logic with the clock injected (every call that needs time
//! takes `now_ms`), so every behaviour is tested on the host against a fake
//! radio:
//!
//! * **`os.lazy.net.wifi.hw.v1` semantics** (`idl/wifi.midl`): one owner
//!   ([`SoftMac::attach`], everyone else `EACCES`, a detach or the owner's
//!   exit leaves the radio idle), the `SetCountry` gate and a regulatory table
//!   ([`regdom`]), scanning as software channel stepping with a dwell and
//!   active probes ([`scan`]), paged raw results with their age, `Join`,
//!   `TxMgmt` (management frames checked and sequenced, EAPOL wrapped in a
//!   data frame, protected once a pairwise key exists), `SetState` (the
//!   network card's link is up only in `Authorized`), `SetKey`/`DelKey`,
//!   `Leave`, the events (`ScanDone`, `RxMgmt`, `BeaconLoss` counted from the
//!   joined BSS's missed beacons, `Deauthenticated`) and `Stats`.
//! * **the data path** under `os.lazy.net.nic.v1`: Ethernet frames to and
//!   from 802.11 data frames (`ieee80211::data`), the CCMP header with a
//!   transmit packet number per key, a replay check per key and TID on
//!   receive, and the controlled port: nothing but EAPOL passes before
//!   `Authorized`, and EAPOL never reaches the network stack.
//!
//! The serving code (Messenger, rings) is the radio program's
//! (`user/src/wifi/`); the frame formats are `libs/ieee80211`'s. Nothing
//! here parses an information element except to learn the joined BSS's
//! beacon interval and whether it speaks QoS, through the fuzzed parser.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod assoc;
pub mod events;
mod keys;
mod mac;
pub mod radio;
pub mod regdom;
mod rx;
pub mod scan;
mod scanning;
mod tx;

#[cfg(test)]
mod tests;

pub use assoc::{JoinRequest, KeyRequest};
pub use events::{Event, FrameKind, Stats};
pub use mac::{HwFacts, SoftMac};
pub use radio::{
    Band, Channel, KeyInstall, KeyKind, Radio, RadioError, RadioInfo, Received, StaState,
    TxOptions, Width,
};
pub use scanning::ScanRequest;
pub use tx::DataDrop;

/// Why a `hw.v1` call was refused. [`Error::errno`] is the code the service
/// answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Already attached, or a scan is running.
    Busy,
    /// The caller is not the attached owner.
    Denied,
    /// No country yet: nothing may be transmitted.
    Again,
    /// A malformed request or one the radio or the domain cannot do.
    Invalid,
    /// An unknown scan id.
    NotFound,
    /// Not joined to a BSS.
    NotConnected,
    /// No free key slot, or the key's packet numbers are spent.
    NoSpace,
    /// The radio failed.
    Radio,
}

impl Error {
    /// The Linux errno value (`EBUSY`, `EACCES`, `EAGAIN`, `EINVAL`,
    /// `ENOENT`, `ENOTCONN`, `ENOSPC`, `EIO`).
    pub const fn errno(self) -> i64 {
        match self {
            Error::Busy => 16,
            Error::Denied => 13,
            Error::Again => 11,
            Error::Invalid => 22,
            Error::NotFound => 2,
            Error::NotConnected => 107,
            Error::NoSpace => 28,
            Error::Radio => 5,
        }
    }
}
