//! Judging one path against a label's granted rule ids.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{home_root_segment, home_subject, literal_subject, rule_id, Kind, MAX_PATH_BYTES};

/// What an operation needs of a path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    /// Existence and metadata.
    Stat,
    /// Data, a directory listing, execution.
    Read,
    /// Create, remove, rename, truncate, write, change attributes.
    Write,
}

/// Split an absolute normalised path into its segments. `None` for a path a
/// caller should never hand over (relative, `.`/`..`/empty segments, NUL,
/// too long): the answer for those is "no".
fn split(path: &str) -> Option<Vec<&str>> {
    if path.len() > MAX_PATH_BYTES || path.as_bytes().contains(&0) {
        return None;
    }
    let rest = path.strip_prefix('/')?;
    if rest.is_empty() {
        return Some(Vec::new());
    }
    let segments: Vec<&str> = rest.split('/').collect();
    if segments
        .iter()
        .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        return None;
    }
    Some(segments)
}

/// What every confined task may do without a rule: the data devices and the
/// fabricated `/proc` (machine and own-process descriptions, no user data).
/// `segments` are the path's.
pub fn baseline(segments: &[&str], access: Access) -> bool {
    let writing = access == Access::Write;
    match segments {
        // The root is looked at (a path walk), never listed: its names are
        // the machine's layout.
        [] => access == Access::Stat,
        ["dev"] | ["proc"] => !writing,
        ["dev", "null" | "zero" | "full"] => true,
        ["dev", "random" | "urandom"] => !writing,
        ["proc", ..] => !writing,
        _ => false,
    }
}

/// Whether a confined task may do `access` to `path`, given `granted`: does
/// the label hold the allow rule with this id. `path` is absolute and
/// lexically normalised; anything else is refused.
pub fn allows(path: &str, access: Access, granted: &dyn Fn(u64) -> bool) -> bool {
    let Some(segments) = split(path) else {
        return false;
    };
    if baseline(&segments, access) {
        return true;
    }
    let home = segments
        .first()
        .is_some_and(|first| *first == home_root_segment());
    let inside = |literal: &str, home_form: Option<&str>, kind: Kind| {
        granted(rule_id(kind, literal)) || home_form.is_some_and(|s| granted(rule_id(kind, s)))
    };
    // Every ancestor-or-self prefix: a grant on one covers the path.
    for end in 0..=segments.len() {
        let literal: String = literal_subject(&segments[..end]);
        // `/home/<name>/rest` is also `$HOME/rest`.
        let home_form: Option<String> = (home && end >= 2).then(|| home_subject(&segments[2..end]));
        let home_form = home_form.as_deref();
        let covered = match access {
            Access::Write => inside(&literal, home_form, Kind::Write),
            Access::Read | Access::Stat => {
                inside(&literal, home_form, Kind::Read) || inside(&literal, home_form, Kind::Write)
            }
        };
        if covered {
            return true;
        }
        // The path itself is the ancestor of a grant: it may be looked at.
        if access == Access::Stat
            && end == segments.len()
            && inside(&literal, home_form, Kind::Trav)
        {
            return true;
        }
    }
    false
}
