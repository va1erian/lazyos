//! Bluetooth LE Security Manager Protocol for the host (central) role:
//! PDU codecs, the SMP crypto functions and the LE Secure Connections
//! pairing state machine (docs/bluetooth-plan.md B4, decisions 6 and 8).
//!
//! The crate has no I/O and no clock. A service (`btd`) owns the L2CAP SMP
//! channel (CID 6) and the HCI link and drives [`Central`] with bytes in,
//! bytes out:
//!
//! ```text
//! let mut smp = Central::new(config, entropy);
//! let step = smp.step(now_ms, Input::Start);      // send step.send, show notices
//! let step = smp.step(now_ms, Input::Pdu(&sdu));  // for each SMP SDU received
//! // on Notice::StartEncryption: HCI LE Start Encryption with the LTK, then
//! let step = smp.step(now_ms, Input::Encryption { ok: true });
//! // step.outcome becomes Some(Ok(Bond)) when the keys are complete
//! ```
//!
//! What it does and refuses:
//!
//! * LE Secure Connections only. A peer that does not set the SC bit gets
//!   [`Error::LegacyPairingRefused`] (plan Risk 3: legacy `c1`/`s1` is left
//!   out until a device in hand needs it).
//! * Just Works, Passkey Entry (the host shows six digits, the peer keyboard
//!   types them; 20 rounds of the bit commitment) and Numeric Comparison.
//! * Peer public keys are validated on the curve (CVE-2018-5383), the SIG
//!   debug key and our own key reflected back are refused, and confirm and
//!   check values compare in constant time.
//! * Encryption key size below [`Config::min_key_size`] (16) is refused.
//! * The 30 s SMP timeout is a deadline in every [`Step`]; there is no timer
//!   inside.
//! * Keys never print: [`Secret`] and every type holding one redact `Debug`.
//!
//! Byte order: structs and PDUs keep air order (little-endian); `func` takes
//! the specification's most-significant-first form so the published vectors
//! paste in unchanged; `pairing` reverses at that one boundary.

#![cfg_attr(not(any(test, feature = "fuzz")), no_std)]

extern crate alloc;

mod auth;
mod dist;
pub mod func;
mod method;
pub mod pairing;
pub mod pdu;
mod secret;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
#[cfg(test)]
mod tests;

pub use method::Method;
pub use pairing::{
    Bond, Central, Config, DeviceAddr, Input, LocalIdentity, Notice, Step, SMP_TIMEOUT_MS,
};
pub use pdu::{AddrType, AuthReq, DecodeError, IoCapability, PairingFeatures, Pdu, Reason};
pub use secret::Secret;

/// Why a pairing ended without a bond. A short, specific variant per cause so
/// the caller (and a test) can tell a wrong passkey from a hostile peer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// No PDU for 30 s. The link should be dropped; no Failed PDU is sent.
    Timeout,
    /// The peer sent Pairing Failed.
    PeerFailed(Reason),
    /// The caller cancelled.
    Cancelled,
    /// The peer does not do LE Secure Connections (plan Risk 3).
    LegacyPairingRefused,
    /// The negotiated encryption key size is below the configured minimum.
    KeySizeTooSmall { size: u8 },
    /// The peer wants out-of-band data, which is not supported.
    OobUnsupported,
    /// The IO capabilities give an unauthenticated key and the caller
    /// required an authenticated one.
    AuthenticationRequired,
    /// A confirm value did not match: wrong passkey, or an active attacker.
    ConfirmMismatch,
    /// The DHKey check did not match.
    DhKeyCheckMismatch,
    /// The user said the numbers differ.
    UserRejected,
    /// The peer's public key is not a point on P-256.
    InvalidPublicKey,
    /// The peer used the SIG debug key (whose private key is public) or
    /// echoed our own key back.
    WeakPublicKey,
    /// The key distribution fields ask for more than we offered.
    KeyDistributionMismatch,
    /// The peer's identity address is not a valid public or static address.
    InvalidIdentity,
    /// A PDU that does not decode.
    Malformed(DecodeError),
    /// A valid PDU the current state does not allow.
    UnexpectedPdu { code: u8 },
    /// The link did not encrypt with the new key.
    EncryptionFailed,
    /// The entropy source has no seed; a pairing from it would be guessable.
    Unseeded,
}
