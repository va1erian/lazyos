//! Who may do what with a named secret, decided from the Messenger sender's
//! kernel-stamped credentials alone.
//!
//! Never from a uid 0 or a capability (AGENTS.md: "Nobody is root"): a
//! `system` secret changes only with `elevd`'s approval, and a PMK goes to
//! `wlanmd` and nobody else.

use crate::{Owner, ELEVD_UID, WLAN_UID};

/// A caller's kernel-stamped credentials (`Message::caller`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caller {
    pub uid: u32,
    pub label_id: u32,
    pub session: u64,
}

impl Caller {
    /// A system service: the uid it runs as, unlabelled, outside any login
    /// session. The kernel stamps all three, so none can be claimed.
    fn is_service(&self, uid: u32) -> bool {
        self.uid == uid && self.label_id == 0 && self.session == 0
    }

    /// `elevd`, after an administrator approved what it asks.
    pub fn is_elevd(&self) -> bool {
        self.is_service(ELEVD_UID)
    }

    /// `wlanmd`, the station manager.
    pub fn is_wlan(&self) -> bool {
        self.is_service(WLAN_UID)
    }
}

/// What a caller asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Store,
    Delete,
    List,
    /// The PMK of the secret `owner_uid` owns (ignored for `system`).
    Pmk {
        owner_uid: u32,
    },
}

/// Why a request is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// `EPERM`: this caller may not do this.
    Perm,
    /// `EINVAL`: not a scope, or a `system` PMK that names a user.
    Invalid,
}

/// Whose secret the request is about, or why it is refused.
///
/// | scope | store, delete | list | PMK |
/// |---|---|---|---|
/// | `user` | the caller's own | the caller's own | `wlanmd`, for the uid it names |
/// | `system` | `elevd` only | anyone (names only) | `wlanmd` |
pub fn authorize(caller: Caller, scope: &str, op: Op) -> Result<Owner, Denied> {
    let system = match scope {
        "user" => false,
        "system" => true,
        _ => return Err(Denied::Invalid),
    };
    match (op, system) {
        (Op::Pmk { .. }, _) if !caller.is_wlan() => Err(Denied::Perm),
        (Op::Pmk { owner_uid }, false) => Ok(Owner::User(owner_uid)),
        (Op::Pmk { owner_uid: 0 }, true) => Ok(Owner::System),
        (Op::Pmk { .. }, true) => Err(Denied::Invalid),
        (Op::Store | Op::Delete, true) if !caller.is_elevd() => Err(Denied::Perm),
        (_, true) => Ok(Owner::System),
        (_, false) => Ok(Owner::User(caller.uid)),
    }
}
