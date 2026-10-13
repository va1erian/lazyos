//! Default apps (docs/lazywebplus.md): which of two apps that handle a type opens it, per user.
//!
//! The open-with registry (`mimed`) keeps one app per `(type, verb)` and lets whichever
//! registered last win, which is install order: LazyWeb and LazyWeb+ both register
//! `x-scheme-handler/https`. A user who minds says so in `confd`, in their own subtree:
//!
//! ```text
//! user/<uid>/mimed/default/<mime type>      a string: the app id (`os.lazy.lazywebplus`)
//! ```
//!
//! The Settings app writes the key, `mimed` reads it for the uid the kernel stamped on an `Open`
//! and honours it only while that app still handles the type. `confd` lets only the uid itself
//! (and system services) touch `user/<uid>/**`, so one account's choice never moves another's. A
//! `confd` segment is `[a-z0-9_.-]`: a type that needs another character (`image/svg+xml`) has no
//! key, and so no preference.

use alloc::format;
use alloc::string::String;
use confd::Value;

use crate::valid_system_name;

/// The types a web browser handles: Settings sets all three at once.
pub const WEB_TYPES: [&str; 3] = [
    "x-scheme-handler/http",
    "x-scheme-handler/https",
    "text/html",
];

/// The apps Settings offers as the default web browser (the ones that register [`WEB_TYPES`]), in
/// the order it lists them.
pub const BROWSERS: [&str; 2] = ["os.lazy.lazyweb", "os.lazy.lazywebplus"];

/// `uid`'s key for `mime`; `None` when `mime` cannot be part of a `confd` path.
pub fn user_key(uid: u32, mime: &str) -> Option<String> {
    let mime = mime.trim().to_ascii_lowercase();
    let (kind, subtype) = mime.split_once('/')?;
    let segment_ok = |part: &str| {
        !part.is_empty()
            && part != ".."
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
    };
    (segment_ok(kind) && segment_ok(subtype)).then(|| format!("user/{uid}/mimed/default/{mime}"))
}

/// The app a stored value names: only a string holding a well-formed app id counts (storage is
/// untrusted: a corrupt value must not choose an app).
pub fn app(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::Str(app)) if valid_system_name(app) => Some(app),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn a_key_is_the_users_own_path_for_a_type() {
        assert_eq!(
            user_key(1000, "x-scheme-handler/https").as_deref(),
            Some("user/1000/mimed/default/x-scheme-handler/https")
        );
        assert_eq!(
            user_key(7, " Text/HTML ").as_deref(),
            Some("user/7/mimed/default/text/html")
        );
    }

    #[test]
    fn a_type_that_cannot_be_a_path_has_no_key() {
        for mime in [
            "image/svg+xml",
            "a/../b",
            "nosubtype",
            "a/b/c",
            "",
            "/b",
            "a/",
            "a b/c",
            "a/b\u{e9}",
        ] {
            assert_eq!(user_key(1, mime), None, "{mime:?}");
        }
    }

    #[test]
    fn every_web_type_has_a_key_and_every_browser_is_an_app_id() {
        for mime in WEB_TYPES {
            assert!(user_key(1000, mime).is_some(), "{mime}");
        }
        for browser in BROWSERS {
            assert!(valid_system_name(browser), "{browser}");
        }
    }

    #[test]
    fn only_a_well_formed_app_id_in_a_string_counts() {
        assert_eq!(
            app(Some(&Value::Str("os.lazy.lazywebplus".to_string()))),
            Some("os.lazy.lazywebplus")
        );
        assert_eq!(app(Some(&Value::Str("../etc".to_string()))), None);
        assert_eq!(app(Some(&Value::Str(String::new()))), None);
        assert_eq!(app(Some(&Value::Bool(true))), None);
        assert_eq!(app(Some(&Value::U64(1))), None);
        assert_eq!(app(None), None);
    }
}
