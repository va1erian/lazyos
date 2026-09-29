//! Path grammar and the uid access rules built on top of it.
//!
//! v1 has exactly two subtrees, so path checking and authorisation are
//! deliberately a pure function of the string and the caller's uid — no ACL
//! table, no ambient state.

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

/// Whether `uid` may read `path` (which must be valid: `sys/**` is public,
/// `user/<uid>/**` is owner-or-root, everything else is denied).
pub(crate) fn can_read(path: &str, uid: u32) -> bool {
    match scope(path) {
        Scope::System => true,
        Scope::User(owner) => uid == owner || uid == 0,
        Scope::Unclaimed => false,
    }
}

/// Whether `uid` may write `path` (`sys/**` is root-only,
/// `user/<uid>/**` is owner-or-root, everything else is denied).
pub(crate) fn can_write(path: &str, uid: u32) -> bool {
    match scope(path) {
        Scope::System => uid == 0,
        Scope::User(owner) => uid == owner || uid == 0,
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
        assert!(can_read("sys/a", 1234));
        assert!(!can_write("sys/a", 1234));
        assert!(can_write("sys/a", 0));

        assert!(can_read("user/7/a", 7));
        assert!(can_read("user/7/a", 0));
        assert!(!can_read("user/7/a", 8));
        assert!(can_write("user/7/a", 7));
        assert!(can_write("user/7/a", 0));
        assert!(!can_write("user/7/a", 8));

        assert!(!can_read("user", 0));
        assert!(!can_write("user", 0));
        assert!(!can_read("user/alice", 0));
        assert!(!can_write("user/alice", 0));
    }
}
