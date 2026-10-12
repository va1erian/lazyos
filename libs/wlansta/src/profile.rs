//! Profiles as `confd` keys (docs/wifi-prerequisites-plan.md section 3.7):
//!
//! ```text
//! sys/net/wifi/networks/<id>/{ssid,security,hidden,autoconnect,priority}
//! user/<uid>/net/wifi/networks/<id>/{...}
//! ```
//!
//! Non-secret fields only; the passphrase is `keyd`'s, under the same `<id>`
//! in the matching scope. An id is `w` and 12 lowercase hex digits: a valid
//! `confd` path segment and a valid `keyd` secret name.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::types::{Owner, Profile, Security};

/// Where system profiles live.
pub const SYSTEM_PREFIX: &str = "sys/net/wifi/networks";
/// The tail of a user's profile prefix, after `user/<uid>`.
pub const USER_TAIL: &str = "net/wifi/networks";
/// The regulatory domain key (ISO 3166 alpha-2, `00` for the world domain).
pub const COUNTRY_KEY: &str = "sys/net/wifi/country";
/// The world domain, used when the key is unset or not a country code.
pub const WORLD: &str = "00";
/// Profiles one owner (the system, or one user) keeps at most.
pub const MAX_PER_OWNER: usize = 32;
/// Ids of one owner read at most per load: a user who writes thousands of
/// junk keys under their own subtree costs bounded work, and invalid ones
/// (which [`parse`] drops) do not take the place of valid ones below this.
pub const MAX_READ_PER_OWNER: usize = 2 * MAX_PER_OWNER;

/// A field's value, mirroring the `confd` kinds used here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Bool(bool),
    U64(u64),
}

/// The prefix of `owner`'s profiles.
pub fn prefix(owner: Owner) -> String {
    match owner {
        Owner::System => String::from(SYSTEM_PREFIX),
        Owner::User(uid) => format!("user/{uid}/{USER_TAIL}"),
    }
}

/// The key of one field of a profile.
pub fn key(owner: Owner, id: &str, field: &str) -> String {
    format!("{}/{id}/{field}", prefix(owner))
}

/// A fresh id from six random octets.
pub fn new_id(random: [u8; 6]) -> String {
    let mut id = String::from("w");
    for byte in random {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// Whether `id` is one this service makes (and so a safe path segment and
/// secret name): `w` and 12 lowercase hex digits.
pub fn valid_id(id: &str) -> bool {
    id.len() == 13
        && id.starts_with('w')
        && id[1..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The fields of `profile`, in the order they are written (`ssid` last, so
/// a reader that sees an `ssid` sees the rest).
pub fn fields(profile: &Profile) -> Vec<(&'static str, Value)> {
    let security = match profile.security {
        Security::Wpa2Psk => "wpa2-psk",
        _ => "open",
    };
    alloc::vec![
        ("security", Value::Str(String::from(security))),
        ("hidden", Value::Bool(profile.hidden)),
        ("autoconnect", Value::Bool(profile.autoconnect)),
        ("priority", Value::U64(u64::from(profile.priority))),
        (
            "ssid",
            Value::Str(String::from_utf8_lossy(&profile.ssid).into_owned())
        ),
    ]
}

/// Every field name, for deletion.
pub const FIELDS: [&str; 5] = ["security", "hidden", "autoconnect", "priority", "ssid"];

/// Read a profile back; `None` when its SSID or security is missing or
/// invalid (a half-written or tampered profile is ignored, not guessed).
pub fn parse(owner: Owner, id: &str, get: impl Fn(&str) -> Option<Value>) -> Option<Profile> {
    if !valid_id(id) {
        return None;
    }
    let Some(Value::Str(ssid)) = get("ssid") else {
        return None;
    };
    if !crate::ssid::valid(ssid.as_bytes()) {
        return None;
    }
    let security = match get("security") {
        Some(Value::Str(text)) if text == "open" => Security::Open,
        Some(Value::Str(text)) if text == "wpa2-psk" => Security::Wpa2Psk,
        _ => return None,
    };
    let flag = |name: &str, default: bool| match get(name) {
        Some(Value::Bool(value)) => value,
        _ => default,
    };
    let priority = match get("priority") {
        Some(Value::U64(value)) => u32::try_from(value).unwrap_or(u32::MAX),
        _ => 0,
    };
    Some(Profile {
        id: String::from(id),
        ssid: ssid.into_bytes(),
        security,
        hidden: flag("hidden", false),
        autoconnect: flag("autoconnect", true),
        priority,
        owner,
    })
}

/// The owner and id a profile key names (`<prefix>/<id>/<field>`), if it is
/// one: a system key, or a user key whose uid parses.
pub fn split_key(key: &str) -> Option<(Owner, &str)> {
    let (owner, rest) = match key.strip_prefix(SYSTEM_PREFIX) {
        Some(rest) => (Owner::System, rest),
        None => {
            let rest = key.strip_prefix("user/")?;
            let (uid, rest) = rest.split_once('/')?;
            let uid: u32 = uid.parse().ok()?;
            (Owner::User(uid), rest.strip_prefix(USER_TAIL)?)
        }
    };
    let (id, field) = rest.strip_prefix('/')?.split_once('/')?;
    (valid_id(id) && FIELDS.contains(&field)).then_some((owner, id))
}

/// The distinct (owner, id) pairs among `keys`, in order of first sight, at
/// most `per_owner` for each owner.
pub fn ids(keys: &[String], per_owner: usize) -> Vec<(Owner, String)> {
    let mut out: Vec<(Owner, String)> = Vec::new();
    for key in keys {
        if let Some((owner, id)) = split_key(key) {
            let known = out.iter().any(|(o, i)| *o == owner && i == id);
            let full = out.iter().filter(|(o, _)| *o == owner).count() >= per_owner;
            if !known && !full {
                out.push((owner, String::from(id)));
            }
        }
    }
    out
}

/// The profiles a station holds, from what was read: system profiles first;
/// a user profile whose id a system profile has, or a second profile of the
/// same owner and id, dropped; at most `per_owner` per owner, counted after
/// parsing. A profile is named by (owner, id) from here on, and a caller's
/// id is unambiguous among the profiles it may use (the system's and its
/// own).
pub fn normalize(profiles: Vec<Profile>, per_owner: usize) -> Vec<Profile> {
    let (system, users): (Vec<Profile>, Vec<Profile>) =
        profiles.into_iter().partition(|p| p.owner == Owner::System);
    let mut out: Vec<Profile> = Vec::new();
    for profile in system.into_iter().chain(users) {
        let clash = out
            .iter()
            .any(|p| p.id == profile.id && (p.owner == profile.owner || p.owner == Owner::System));
        let full = out.iter().filter(|p| p.owner == profile.owner).count() >= per_owner;
        if !clash && !full {
            out.push(profile);
        }
    }
    out
}

/// The country to give the radio: the configured code when it is two
/// uppercase ASCII letters (or `00`), else the world domain.
pub fn country(configured: Option<&str>) -> &str {
    match configured {
        Some(code)
            if code == WORLD
                || (code.len() == 2 && code.bytes().all(|b| b.is_ascii_uppercase())) =>
        {
            code
        }
        _ => WORLD,
    }
}
