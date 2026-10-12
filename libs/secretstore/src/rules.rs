//! Who may do what with a named secret, decided from the Messenger sender's
//! kernel-stamped credentials alone.
//!
//! Never from a uid 0 or a capability (AGENTS.md: "Nobody is root"): a
//! `system` secret changes only with `elevd`'s approval, and a PMK goes to
//! `wlanmd` and nobody else.

use crate::{Owner, ELEVD_UID, WLAN_UID};

/// Length of a Wi-Fi profile id: `w` and 12 lowercase hex digits.
const PROFILE_ID_LEN: usize = 13;

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
    /// Store a secret for the user `owner_uid`: `wlanmd` keeping the
    /// passphrase of a network that user asked it to remember.
    StoreFor {
        owner_uid: u32,
    },
    /// Delete a secret on the same terms.
    DeleteFor {
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

/// Whether `name` is a Wi-Fi profile id, the only secret names `wlanmd`
/// works on: `w` and 12 lowercase hex digits, the form it mints
/// (`wlansta::profile::new_id`, checked by `valid_id`).
pub fn is_wifi_profile_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == PROFILE_ID_LEN
        && bytes[0] == b'w'
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

/// Whose secret the request about `name` is, or why it is refused.
///
/// | scope | store, delete | list | PMK |
/// |---|---|---|---|
/// | `user` | the caller's own | the caller's own | `wlanmd`, for the uid it names |
/// | `system` | `elevd` only | anyone (names only) | `wlanmd` |
///
/// `wlanmd` parses frames off the air, so it is treated as the likeliest
/// service to be compromised and gets no more than Wi-Fi needs. Its three
/// operations ([`Op::Pmk`], [`Op::StoreFor`], [`Op::DeleteFor`]) name a
/// Wi-Fi profile id ([`is_wifi_profile_name`]) or are refused: it never
/// derives a PMK from, overwrites or deletes a secret that is not a Wi-Fi
/// network's (a PMK is a PBKDF2 of the secret, crackable offline). It keeps
/// secrets only in the `user` scope, for the user who added the network
/// (`os.lazy.net.wifi.v1` `AddNetwork` takes the uid from the kernel); a
/// `system` network's passphrase is written by `elevd` itself
/// (`net.wifi.system`), so the `system` scope stays `elevd`'s alone.
pub fn authorize(caller: Caller, scope: &str, name: &str, op: Op) -> Result<Owner, Denied> {
    let system = match scope {
        "user" => false,
        "system" => true,
        _ => return Err(Denied::Invalid),
    };
    match (op, system) {
        (Op::Pmk { .. } | Op::StoreFor { .. } | Op::DeleteFor { .. }, _)
            if !caller.is_wlan() || !is_wifi_profile_name(name) =>
        {
            Err(Denied::Perm)
        }
        (
            Op::Pmk { owner_uid } | Op::StoreFor { owner_uid } | Op::DeleteFor { owner_uid },
            false,
        ) => Ok(Owner::User(owner_uid)),
        (Op::StoreFor { .. } | Op::DeleteFor { .. }, true) => Err(Denied::Perm),
        (Op::Pmk { owner_uid: 0 }, true) => Ok(Owner::System),
        (Op::Pmk { .. }, true) => Err(Denied::Invalid),
        (Op::Store | Op::Delete, true) if !caller.is_elevd() => Err(Denied::Perm),
        (_, true) => Ok(Owner::System),
        (_, false) => Ok(Owner::User(caller.uid)),
    }
}
