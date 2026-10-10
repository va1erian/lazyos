//! EAPOL-Key frames and the WPA2-Personal supplicant
//! (`docs/wifi-prerequisites-plan.md` WP3, section 3.4).
//!
//! * [`key`] parses and builds EAPOL-Key frames ([`KeyFrame`]).
//! * [`keydata`] walks the decrypted key data (RSN element, GTK KDE, padding).
//! * [`crypto`] is the [`Crypto`] trait the state machine reaches the WP0
//!   functions through; [`Standard`] implements it over `lazyos-crypto`.
//! * [`Supplicant`] is the pure state machine: [`Supplicant::input`] takes one
//!   received EAPOL-Key PDU and returns [`Action`]s.
//!
//! Supported: AKM 2 (PSK; HMAC-SHA1-128 MIC, PRF-SHA1) and AKM 6 (PSK-SHA256;
//! AES-128-CMAC MIC, KDF-SHA256), CCMP-128 for pairwise and group. TKIP, WEP,
//! GCMP, SAE, 802.1X and management frame protection are refused with a named
//! [`Error`].
//!
//! Written from IEEE Std 802.11-2020 clause 12.7. No code or structure was
//! taken from any other implementation; see `THIRD_PARTY.md`.
//!
//! # What the caller must do
//!
//! The library has no clock and no I/O. `wlanmd` owns:
//!
//! * **Transport.** Pass the EAPOL PDU (from the EAPOL version octet; strip
//!   the Ethernet header) of frames whose source address is the BSSID. Send
//!   each [`Action::Send`] PDU to the BSSID as EAPOL (ethertype 0x888E).
//! * **Timers.** The 4-way handshake needs a timeout (the standard's
//!   `dot11RSNAConfigPairwiseUpdateTimeOut` is 100 ms with 3 retries for the
//!   authenticator; a supplicant gives up when the whole exchange has not
//!   finished about 1 s after association) and the group handshake likewise.
//!   On expiry deauthenticate (reason 15). The supplicant never retransmits
//!   message 2 or 4 by itself: it answers each message 1 and 3 it receives,
//!   and the authenticator's retransmissions drive it.
//! * **Key installation order.** Execute the actions of one [`Supplicant::input`]
//!   call in order. [`Action::Send`] of message 4 comes before
//!   [`Action::InstallPtk`]: wait until the chip reports message 4 transmitted
//!   (it must go out unencrypted), then install the pairwise key, then the
//!   group key, and only on [`Action::Authorized`] open the data port. Install
//!   a group key only when the action says so; the library suppresses
//!   re-installs of a key it already delivered (see below).
//! * **Errors.** Every `Err` leaves the supplicant exactly as it was. Drop the
//!   frame, or, when [`Error::is_fatal`], deauthenticate with
//!   [`Error::deauth_reason`]; the frame was authenticated and the AP is
//!   either broken or downgrading.
//!
//! # Replays and key reinstallation
//!
//! The replay counter must strictly increase over every MIC-verified frame.
//! A retransmitted message 3 (the AP did not see our message 4) is answered
//! with message 4 again and installs nothing: reinstalling a pairwise key
//! resets its packet number (the KRACK weakness). A group message 1 that
//! delivers the key already installed is answered but not reinstalled.
//!
//! # Message 1, ANonce and SNonce
//!
//! A new message 1 with an ANonce the supplicant has not seen gets a new
//! SNonce and a new pending PTK; the installed PTK stays in use until message
//! 3 verifies (PTK rekey). A retransmitted message 1 (same ANonce) reuses the
//! SNonce and PTK, so the message 3 that follows either copy of message 2
//! verifies. A message 1 whose ANonce equals the ANonce of the PTK already
//! installed is refused ([`Error::StaleAnonce`]): it is a replay, since an
//! authenticator uses a fresh ANonce for each handshake.
//!
//! Message 1 is not authenticated, so the supplicant keeps up to
//! [`MAX_PENDING`] (4) candidates, each `(ANonce, SNonce, PTK, counter)`. Message
//! 3 selects the candidate by its ANonce and is MIC-verified with that
//! candidate's PTK; the other candidates are discarded only after a verified
//! message 3. A forged message 1 therefore cannot break a genuine exchange in
//! progress, and a forged message 3 fails its MIC. What remains: a flood of
//! more than [`MAX_PENDING`] forged message 1s evicts the oldest candidates, the
//! genuine one included, so the exchange fails and the caller's timer and the
//! authenticator's retry start over (the newest exchange is always kept); the
//! flood also costs one PTK derivation and one message 2 per forged frame.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod crypto;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
mod group;
mod handshake;
pub mod key;
pub mod keydata;
mod secret;
mod supplicant;
#[cfg(test)]
mod tests;

use alloc::vec::Vec;

pub use crypto::{Crypto, Standard};
pub use ieee80211::{Akm, Cipher};
pub use key::KeyFrame;
pub use secret::Key;
pub use supplicant::{Config, Stage, State, Supplicant, MAX_PENDING};

/// Something the caller must do after [`Supplicant::input`].
#[derive(Debug)]
#[cfg_attr(any(test, feature = "fuzz"), derive(PartialEq))]
pub enum Action {
    /// Transmit this EAPOL PDU to the AP.
    Send(Vec<u8>),
    /// Install the pairwise temporal key (after message 4 has gone out).
    InstallPtk { cipher: Cipher, tk: Key<16> },
    /// Install a group temporal key. `rsc` is the receive sequence counter the
    /// AP stated (CCMP packet number, little-endian octets).
    InstallGtk {
        index: u8,
        tx: bool,
        key: Key<16>,
        rsc: [u8; 8],
    },
    /// The handshake is complete: open the controlled port.
    Authorized,
}

/// Why a frame was refused, or a configuration rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Fewer octets than the header and body length need.
    Short,
    /// An EAPOL packet that is not an EAPOL-Key frame.
    NotKey,
    /// The body length disagrees with the key data length.
    BadLength,
    /// The key descriptor is not RSN (type 2); 254 is WPA1.
    UnsupportedDescriptor(u8),
    /// The key descriptor version does not match the AKM.
    BadVersion(u8),
    /// Key information flags that fit none of the expected messages.
    BadKeyInfo(u16),
    /// A Key Length field other than the CCMP key length.
    BadKeyLength(u16),
    /// A message that does not fit the current stage (message 3 before
    /// message 1, a group message before the pairwise key).
    Unexpected,
    /// The MIC does not verify.
    BadMic,
    /// The replay counter did not increase.
    Replay,
    /// Message 3's ANonce differs from message 1's.
    AnonceChanged,
    /// Message 1 carries the ANonce of the installed PTK.
    StaleAnonce,
    /// An all-zero ANonce.
    ZeroAnonce,
    /// Key data that does not unwrap, or is malformed.
    BadKeyData,
    /// No GTK KDE where one is required.
    MissingGtk,
    /// A GTK KDE of the wrong size for CCMP.
    BadGtk,
    /// Message 3 carries no RSN element.
    MissingRsn,
    /// Message 3's RSN element differs from the beacon's: a downgrade.
    RsnMismatch,
    /// The AKM is not 2 or 6.
    UnsupportedAkm(Akm),
    /// The cipher is not CCMP-128 (TKIP and WEP included).
    UnsupportedCipher(Cipher),
    /// The AP's RSN element does not offer the chosen AKM or ciphers, or does
    /// not parse.
    NotOffered,
    /// The AP requires management frame protection, which is unsupported.
    PmfRequired,
    /// The association request's RSN element does not match the choice.
    AssocIeMismatch,
}

impl Error {
    /// A short explanation for logs and the user.
    pub const fn message(self) -> &'static str {
        match self {
            Error::Short => "the EAPOL frame is too short",
            Error::NotKey => "not an EAPOL-Key frame",
            Error::BadLength => "the EAPOL body length disagrees with the key data length",
            Error::UnsupportedDescriptor(_) => {
                "the key descriptor is not RSN (WPA1 is unsupported)"
            }
            Error::BadVersion(_) => "the key descriptor version does not match the AKM",
            Error::BadKeyInfo(_) => "unexpected key information flags",
            Error::BadKeyLength(_) => "the key length is not the CCMP key length",
            Error::Unexpected => "a handshake message out of order",
            Error::BadMic => "the EAPOL-Key MIC is wrong (wrong passphrase or a forged frame)",
            Error::Replay => "the replay counter did not increase",
            Error::AnonceChanged => "message 3 does not carry the ANonce of message 1",
            Error::StaleAnonce => "message 1 repeats the ANonce of the installed key",
            Error::ZeroAnonce => "the ANonce is all zero",
            Error::BadKeyData => "the key data is malformed",
            Error::MissingGtk => "message 3 carries no group key",
            Error::BadGtk => "the group key KDE has the wrong size",
            Error::MissingRsn => "message 3 carries no RSN element",
            Error::RsnMismatch => "message 3's RSN element differs from the beacon's",
            Error::UnsupportedAkm(_) => "the AKM is not PSK or PSK-SHA256",
            Error::UnsupportedCipher(_) => "only CCMP-128 is supported (not TKIP, WEP or GCMP)",
            Error::NotOffered => "the access point does not offer that AKM and cipher",
            Error::PmfRequired => "the access point requires management frame protection",
            Error::AssocIeMismatch => "the association RSN element does not match the choice",
        }
    }

    /// True for a failure of an authenticated frame (the MIC verified) or of
    /// the configuration: the exchange cannot succeed, so deauthenticate.
    /// False means drop the frame and let the timer decide.
    pub const fn is_fatal(self) -> bool {
        matches!(
            self,
            Error::BadKeyData
                | Error::MissingGtk
                | Error::BadGtk
                | Error::MissingRsn
                | Error::RsnMismatch
                | Error::UnsupportedAkm(_)
                | Error::UnsupportedCipher(_)
                | Error::NotOffered
                | Error::PmfRequired
                | Error::AssocIeMismatch
        )
    }

    /// The reason code for the deauthentication frame (Table 9-49) when
    /// [`Error::is_fatal`].
    pub const fn deauth_reason(self) -> Option<u16> {
        match self {
            Error::RsnMismatch | Error::MissingRsn => Some(17),
            Error::UnsupportedCipher(_) | Error::NotOffered => Some(19),
            Error::UnsupportedAkm(_) => Some(20),
            Error::PmfRequired => Some(24),
            Error::AssocIeMismatch => Some(13),
            Error::BadKeyData | Error::MissingGtk | Error::BadGtk => Some(1),
            _ => None,
        }
    }
}
