//! Path grammar and the uid access rules built on top of it.
//!
//! v1 has exactly two subtrees, so path checking and authorisation are
//! deliberately a pure function of the string and the caller (its uid and
//! whether it is a system service) — no ACL table, no ambient state.

use crate::store::Caller;
use crate::{Error, MAX_PATH_LEN};

/// Checks `path` against the v1 grammar.
///
/// Accepts exactly `sys`/`sys/...` or `user`/`user/...` where every segment
/// is non-empty and made of `[a-z0-9_.-]`, the whole path is at most
/// [`MAX_PATH_LEN`] bytes, and no segment is `..`. `user` alone is
/// syntactically valid, but the access rules deny it to every caller.
///
/// # Errors
///
/// Returns [`Error::BadPath`] for anything else; it never panics and never
/// inspects bytes past the end of the path.
pub fn validate_path(path: &str) -> Result<(), Error> {
    if path.is_empty() || path.len() > MAX_PATH_LEN {
        return Err(Error::BadPath);
    }
    let mut segments = path.split('/');
    let top = segments.next().unwrap_or("");
    if top != "sys" && top != "user" {
        return Err(Error::BadPath);
    }
    for segment in segments {
        if segment.is_empty() || segment == ".." || !segment.bytes().all(is_allowed) {
            return Err(Error::BadPath);
        }
    }
    Ok(())
}

/// The allowed segment alphabet. `/` is the separator, so it is excluded by
/// construction, and `..` needs an explicit check in [`validate_path`]
/// because `.` is allowed.
fn is_allowed(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-')
}

/// The subtree a syntactically valid path belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    /// `sys` or `sys/...`: shared system configuration.
    System,
    /// `user/<uid>` or below: owned by the parsed uid.
    User(u32),
    /// Syntactically valid but owned by nobody: `user`, `user/<name>`, or a
    /// uid that does not fit `u32`. Default-deny keeps a typo from becoming a
    /// permissive path.
    Unclaimed,
}

/// Classifies a path that has already passed [`validate_path`].
///
/// The owner segment is parsed as a `u32`, so its spelling is not canonical:
/// `user/0100` and `user/100` both name uid 100, as distinct keys. That is
/// harmless because both are owned by the same uid either way.
pub(crate) fn scope(path: &str) -> Scope {
    if path == "sys" || path.starts_with("sys/") {
        return Scope::System;
    }
    match path.strip_prefix("user/") {
        None => Scope::Unclaimed,
        Some(rest) => match rest.split('/').next().unwrap_or(rest).parse::<u32>() {
            Ok(owner) => Scope::User(owner),
            Err(_) => Scope::Unclaimed,
        },
    }
}

/// Whether `caller` may read `path` (which must be valid: `sys/**` is
/// public, `user/<uid>/**` is its owner's or a system service's, everything
/// else is denied).
pub(crate) fn can_read(path: &str, caller: Caller) -> bool {
    match scope(path) {
        Scope::System => true,
        Scope::User(owner) => caller.uid == owner || caller.system || wifi_delegate(path, caller),
        Scope::Unclaimed => false,
    }
}

/// The subtree a Wi-Fi profile lives in, below `sys` or `user/<uid>`.
const WIFI_PROFILES: &str = "net/wifi/networks";

/// Whether `path` (valid) is a Wi-Fi profile key or its subtree root:
/// `sys/net/wifi/networks[/...]` or `user/<uid>/net/wifi/networks[/...]`.
fn is_wifi_profile(path: &str) -> bool {
    let rest = match path.strip_prefix("sys/") {
        Some(rest) => rest,
        None => match path.strip_prefix("user/").and_then(|r| r.split_once('/')) {
            Some((owner, rest)) if owner.parse::<u32>().is_ok() => rest,
            _ => return false,
        },
    };
    match rest.strip_prefix(WIFI_PROFILES) {
        Some(tail) => tail.is_empty() || tail.starts_with('/'),
        None => false,
    }
}

/// The station manager's delegation: the Wi-Fi profiles, whoever owns them.
fn wifi_delegate(path: &str, caller: Caller) -> bool {
    caller.wifi && is_wifi_profile(path)
}

/// Whether `caller` may write `path` (`sys/**` is the system services'
/// alone, `user/<uid>/**` its owner's or a system service's, everything else
/// is denied). A uid never grants it (issue #623): a root login session holds
/// no capability and writes only `user/0/**`.
pub(crate) fn can_write(path: &str, caller: Caller) -> bool {
    match scope(path) {
        Scope::System => caller.system || wifi_delegate(path, caller),
        Scope::User(owner) => caller.uid == owner || caller.system || wifi_delegate(path, caller),
        Scope::Unclaimed => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_parses_owner() {
        assert_eq!(scope("sys"), Scope::System);
        assert_eq!(scope("sys/net/eth0"), Scope::System);
        assert_eq!(scope("user/1000"), Scope::User(1000));
        assert_eq!(scope("user/1000/shell"), Scope::User(1000));
        assert_eq!(scope("user"), Scope::Unclaimed);
        assert_eq!(scope("user/alice/x"), Scope::Unclaimed);
        assert_eq!(scope("user/4294967296"), Scope::Unclaimed);
        // The owner segment is numeric, not textual: different spellings of
        // the same number name the same owner.
        assert_eq!(scope("user/0100"), Scope::User(100));
    }

    #[test]
    fn access_matrix() {
        let user = Caller::user;
        let system = Caller::system(0);
        assert!(can_read("sys/a", user(1234)));
        assert!(!can_write("sys/a", user(1234)));
        assert!(can_write("sys/a", system));
        // A root login session is no system service.
        assert!(!can_write("sys/a", user(0)));
        assert!(!can_read("user/7/a", user(0)));

        assert!(can_read("user/7/a", user(7)));
        assert!(can_read("user/7/a", system));
        assert!(!can_read("user/7/a", user(8)));
        assert!(can_write("user/7/a", user(7)));
        assert!(can_write("user/7/a", system));
        assert!(!can_write("user/7/a", user(8)));
        assert!(can_write("user/0/a", user(0)));

        assert!(!can_read("user", system));
        assert!(!can_write("user", system));
        assert!(!can_read("user/alice", system));
        assert!(!can_write("user/alice", system));
    }

    #[test]
    fn the_station_manager_reaches_the_wifi_profiles_and_nothing_else() {
        let wlan = Caller::wifi(912);
        for path in [
            "sys/net/wifi/networks",
            "sys/net/wifi/networks/w0/ssid",
            "user/1000/net/wifi/networks/w0/ssid",
            "user/0/net/wifi/networks",
        ] {
            assert!(can_read(path, wlan) && can_write(path, wlan), "{path}");
        }
        for path in [
            "sys/net/wifi/country",
            "sys/net/wifi",
            "sys/net/wifi/networksx",
            "sys/net/eth0/mode",
            "user/1000/net/wifi/other",
            "user/1000/net/wifi/networksx/a",
            "user/1000/settings",
            "user/abc/net/wifi/networks/x",
            "user",
        ] {
            assert!(!can_write(path, wlan), "{path}");
        }
        assert!(!can_read("user/1000/settings", wlan));
        assert!(
            can_write("user/912/anything", wlan),
            "its own subtree, as for any uid"
        );
        // A user still owns their own profiles, and no one else's.
        assert!(can_write(
            "user/1000/net/wifi/networks/w0/ssid",
            Caller::user(1000)
        ));
        assert!(!can_write(
            "user/1000/net/wifi/networks/w0/ssid",
            Caller::user(1001)
        ));
        assert!(!can_write(
            "sys/net/wifi/networks/w0/ssid",
            Caller::user(1000)
        ));
    }
}
