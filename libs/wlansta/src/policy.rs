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
//! | `Disconnect` | a system network (or none): anyone; a user network: its owner |
//! | `Forget` of a user network | its owner |
//!
//! A profile is named by its owner **and** its id: two users may hold the
//! same id (each writes their own `confd` subtree), so a request's id is
//! looked up only among the profiles its caller may use ([`resolve`]).

use crate::networks::Viewer;
use crate::types::{Owner, Profile, Security};

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
    /// `ENOENT`: no profile of that id at all.
    Missing,
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

/// Whether the caller may leave the network of the profile in use (`None`
/// when the station holds none): what anyone may connect to, anyone may
/// leave; another user's network only its owner.
pub fn may_disconnect(caller: Caller, current: Option<Owner>) -> bool {
    current.is_none_or(|owner| may_use(caller, owner))
}

/// The profile a caller's `Connect` names: the one of `id` it may use (a
/// system profile, or its own). `Perm` when only another user's profile has
/// that id, `Missing` when none does.
pub fn resolve<'a>(caller: Caller, profiles: &'a [Profile], id: &str) -> Result<&'a Profile, Denied> {
    let mut same = profiles.iter().filter(|p| p.id == id).peekable();
    if same.peek().is_none() {
        return Err(Denied::Missing);
    }
    same.find(|p| may_use(caller, p.owner)).ok_or(Denied::Perm)
}

/// The profile a caller's `Forget` deletes: the one of `id` it may forget.
/// `Ok(None)` when it names nothing the caller can see (gone, or another
/// user's: not an error, and nothing is revealed), `Perm` for a profile it
/// sees but may not delete (a system network, for a user).
pub fn resolve_forget<'a>(
    caller: Caller,
    profiles: &'a [Profile],
    id: &str,
) -> Result<Option<&'a Profile>, Denied> {
    let mut seen = false;
    for profile in profiles.iter().filter(|p| p.id == id) {
        if may_forget(caller, profile.owner) {
            return Ok(Some(profile));
        }
        seen |= may_use(caller, profile.owner);
    }
    if seen {
        Err(Denied::Perm)
    } else {
        Ok(None)
    }
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
/// a passphrase exactly when it needs one ([`valid_passphrase`]).
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

/// A WPA2 passphrase or raw PSK: 8 to 63 octets of UTF-8 text without
/// control characters, or exactly 64 hex digits. IEEE 802.11 Annex J.4 asks
/// for printable ASCII, but access points (and the stations people set them
/// up with) take any UTF-8, and PBKDF2 hashes octets: refusing `café au
/// lait` would only lock the user out of their own network. `keyd` must
/// accept the same set, or a profile could hold a secret it cannot use.
pub fn valid_passphrase(secret: &str) -> bool {
    let bytes = secret.as_bytes();
    let text = (8..=63).contains(&bytes.len()) && !secret.chars().any(char::is_control);
    let hex = bytes.len() == 64 && bytes.iter().all(u8::is_ascii_hexdigit);
    text || hex
}
