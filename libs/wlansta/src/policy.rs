//! Who may do what on `os.lazy.net.wifi.v1`, and what a request must look
//! like (the rules in `idl/wifi.midl`), decided from the kernel-stamped
//! sender alone.
//!
//! | call | who |
//! |---|---|
//! | `Interfaces`, `Status`, `Networks`, `Scan` | any caller the label policy lets reach the service |
//! | `AddNetwork` scope `User` | a user, for themselves |
//! | `AddNetwork` scope `System`, `Forget` of a system network | `elevd` only (the `net.wifi.system` prompt) |
//! | `Connect` | a system network: anyone; a user network: its owner |
//! | `Disconnect` | anyone who may connect |
//! | `Forget` of a user network | its owner |

use crate::networks::Viewer;
use crate::types::{Owner, Security};

/// The sender, as the kernel stamped it. `elevd` is decided by the caller
/// (`elevpolicy::is_elevd`: its uid, unlabelled, outside any session).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    pub elevd: bool,
}

/// Why a request is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// `EPERM`.
    Perm,
    /// `EINVAL`.
    Invalid,
}

/// `os.lazy.net.wifi.v1` `Scope` ordinals.
pub const SCOPE_USER: u32 = 0;
pub const SCOPE_SYSTEM: u32 = 1;

/// The owner a new profile of `scope` gets. `System` only from `elevd`;
/// `elevd` adds nothing for itself.
pub fn add_owner(caller: Caller, scope: u32) -> Result<Owner, Denied> {
    match scope {
        SCOPE_SYSTEM if caller.elevd => Ok(Owner::System),
        SCOPE_USER if !caller.elevd => Ok(Owner::User(caller.uid)),
        SCOPE_SYSTEM | SCOPE_USER => Err(Denied::Perm),
        _ => Err(Denied::Invalid),
    }
}

/// Whether the caller may connect to (or see the id of) `owner`'s network.
pub fn may_use(caller: Caller, owner: Owner) -> bool {
    viewer(caller).sees(owner)
}

/// Whether the caller may forget `owner`'s network.
pub fn may_forget(caller: Caller, owner: Owner) -> bool {
    match owner {
        Owner::System => caller.elevd,
        Owner::User(uid) => !caller.elevd && uid == caller.uid,
    }
}

/// Whose profiles the caller sees in `Networks`.
pub fn viewer(caller: Caller) -> Viewer {
    if caller.elevd {
        Viewer::System
    } else {
        Viewer::User(caller.uid)
    }
}

/// `AddNetwork`'s checks: an SSID of 1 to 32 octets, a joinable security, and
/// a passphrase exactly when it needs one: 8 to 63 printable ASCII
/// characters, or 64 hex digits (a raw PSK). These are also `keyd`'s and
/// PBKDF2's rules, so a profile never holds a secret that cannot be used.
pub fn check_new(ssid: &str, security: Security, passphrase: Option<&str>) -> Result<(), Denied> {
    if !crate::ssid::valid(ssid.as_bytes()) {
        return Err(Denied::Invalid);
    }
    match (security, passphrase) {
        (Security::Open, None) => Ok(()),
        (Security::Wpa2Psk, Some(secret)) if valid_passphrase(secret) => Ok(()),
        _ => Err(Denied::Invalid),
    }
}

/// A WPA2 passphrase (IEEE 802.11 Annex J.4) or raw PSK.
pub fn valid_passphrase(secret: &str) -> bool {
    let bytes = secret.as_bytes();
    let printable = (8..=63).contains(&bytes.len()) && bytes.iter().all(|b| (0x20..=0x7E).contains(b));
    let hex = bytes.len() == 64 && bytes.iter().all(u8::is_ascii_hexdigit);
    printable || hex
}
