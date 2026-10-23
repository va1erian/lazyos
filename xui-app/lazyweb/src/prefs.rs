//! The person's choices that live in `confd` under `user/<uid>/lazyweb/`, read once at start:
//!
//! - `search/template`: the search URL typed words go to, `%s` for the query
//!   ([`crate::address::DEFAULT_SEARCH`] until set; a value that is not an http(s) URL with `%s`
//!   is ignored);
//! - `session/restore`: `always` brings the last page back after a crash without asking
//!   (anything else asks, `session.rs`).
//!
//! Like the ad blocker's switches they are out of reach of the engine's processes (which may write
//! the profile folder), which is why they are not files. The platform reads them before the window
//! opens and only when `confd` already answers: a store call blocks while it is not registered.

use crate::address::{self, DEFAULT_SEARCH};

/// What to do with the page of a session that did not end cleanly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Restore {
    /// Offer it in a bar above the page.
    #[default]
    Ask,
    /// Open it without asking.
    Always,
}

/// The choices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prefs {
    /// A valid search template (`address::valid_template`).
    pub search: String,
    pub restore: Restore,
}

impl Default for Prefs {
    fn default() -> Prefs {
        Prefs {
            search: DEFAULT_SEARCH.to_owned(),
            restore: Restore::Ask,
        }
    }
}

impl Prefs {
    /// The choices from the stored texts (`None`: not set). What does not parse is the default.
    pub fn from_texts(template: Option<&str>, restore: Option<&str>) -> Prefs {
        Prefs {
            search: template
                .map(str::trim)
                .filter(|t| address::valid_template(t))
                .unwrap_or(DEFAULT_SEARCH)
                .to_owned(),
            restore: match restore.map(str::trim) {
                Some(r) if r.eq_ignore_ascii_case("always") => Restore::Always,
                _ => Restore::Ask,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_bad_values_are_the_defaults() {
        assert_eq!(Prefs::from_texts(None, None), Prefs::default());
        let bad = Prefs::from_texts(Some("ftp://x/%s"), Some("sometimes"));
        assert_eq!(bad, Prefs::default());
        assert_eq!(
            Prefs::from_texts(Some("https://no-placeholder/"), None).search,
            DEFAULT_SEARCH
        );
    }

    #[test]
    fn good_values_are_taken() {
        let prefs = Prefs::from_texts(Some(" https://s.example/?q=%s "), Some("Always"));
        assert_eq!(prefs.search, "https://s.example/?q=%s");
        assert_eq!(prefs.restore, Restore::Always);
    }
}
