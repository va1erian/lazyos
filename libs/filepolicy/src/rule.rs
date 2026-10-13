//! The rule grammar and what a rule set compiles to.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::HOME_VAR;
use crate::{confine_id, home_root_segment, home_subject, literal_subject, rule_id, Kind};

/// Longest path a rule may name, in bytes.
pub const MAX_PATH: usize = 512;
/// Most segments a rule's path may have.
pub const MAX_SEGMENTS: usize = 16;

/// What a rule lets a confined task do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grant {
    Read,
    Write,
}

/// Why a rule is refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuleError {
    /// Not `read:` or `write:` followed by an absolute or `$HOME/` path.
    Shape,
    /// A segment with a byte outside `[A-Za-z0-9_.-]`, an empty segment, or
    /// `.` / `..`.
    Segment,
    /// `*` anywhere but as the last segment (a directory already covers
    /// everything inside it, so a trailing `*` is the only wildcard).
    Wildcard,
    /// An absolute path inside a home directory: name it with `$HOME`.
    AbsoluteHome,
    /// Nothing is left of the path (`read:/`, `read:/*`).
    Root,
    /// More than [`MAX_SEGMENTS`] segments or [`MAX_PATH`] bytes.
    TooLong,
}

impl RuleError {
    /// The problem, worded for a person.
    pub fn message(self) -> &'static str {
        match self {
            RuleError::Shape => "is not a read:/write: absolute or $HOME/ path",
            RuleError::Segment => "has a segment that is empty, . or .. or uses other characters than letters, digits, _, . and -",
            RuleError::Wildcard => "may use * only as its last segment",
            RuleError::AbsoluteHome => "names a home directory; write it as $HOME/...",
            RuleError::Root => "names the root directory",
            RuleError::TooLong => "is too deep or too long",
        }
    }
}

/// One parsed rule.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rule {
    pub grant: Grant,
    /// The path is below the running user's home (`$HOME/...`).
    pub home: bool,
    /// The path's segments, below `$HOME` when `home`.
    pub segments: Vec<String>,
}

impl Rule {
    /// The subject the grant is keyed by: `/a/b` or `$HOME/a/b`.
    pub fn subject(&self) -> String {
        let segments: Vec<&str> = self.segments.iter().map(String::as_str).collect();
        if self.home {
            home_subject(&segments)
        } else {
            literal_subject(&segments)
        }
    }

    /// The subjects of every proper ancestor, root first (`/`, `/a`, ...; for
    /// a home rule `/`, `/home`, `$HOME`, `$HOME/a`, ...).
    pub fn ancestor_subjects(&self) -> Vec<String> {
        let segments: Vec<&str> = self.segments.iter().map(String::as_str).collect();
        let mut out = Vec::new();
        if self.home {
            out.push(literal_subject(&[]));
            out.push(literal_subject(&[home_root_segment()]));
            for end in 0..segments.len() {
                out.push(home_subject(&segments[..end]));
            }
        } else {
            for end in 0..segments.len() {
                out.push(literal_subject(&segments[..end]));
            }
        }
        out
    }
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Parse one `read:<path>` / `write:<path>` rule.
pub fn parse(rule: &str) -> Result<Rule, RuleError> {
    let (grant, path) = if let Some(path) = rule.strip_prefix("read:") {
        (Grant::Read, path)
    } else if let Some(path) = rule.strip_prefix("write:") {
        (Grant::Write, path)
    } else {
        return Err(RuleError::Shape);
    };
    if path.len() > MAX_PATH {
        return Err(RuleError::TooLong);
    }
    let (home, rest) = match path.strip_prefix(HOME_VAR) {
        Some(rest) => (true, rest.strip_prefix('/').ok_or(RuleError::Shape)?),
        None => (false, path.strip_prefix('/').ok_or(RuleError::Shape)?),
    };
    if rest.contains(HOME_VAR) {
        return Err(RuleError::Shape);
    }
    if rest.is_empty() {
        return Err(RuleError::Root);
    }
    let mut segments: Vec<&str> = rest.split('/').collect();
    // A trailing `*` names the directory itself: it already covers what is in it.
    if segments.last() == Some(&"*") {
        segments.pop();
    }
    if segments.is_empty() {
        return Err(RuleError::Root);
    }
    if segments.iter().any(|segment| segment.contains('*')) {
        return Err(RuleError::Wildcard);
    }
    if !segments.iter().all(|segment| valid_segment(segment)) {
        return Err(RuleError::Segment);
    }
    if segments.len() > MAX_SEGMENTS {
        return Err(RuleError::TooLong);
    }
    if !home && segments[0] == home_root_segment() {
        return Err(RuleError::AbsoluteHome);
    }
    Ok(Rule {
        grant,
        home,
        segments: segments.iter().map(|segment| segment.to_string()).collect(),
    })
}

/// The ids a rule set compiles to: the confinement marker, each rule's grant
/// and a stat-only grant for every ancestor of it. Sorted and without
/// duplicates, so equal rule sets compile to equal lists. A refused rule is
/// reported with its index.
pub fn compile<S: AsRef<str>>(rules: &[S]) -> Result<Vec<u64>, (usize, RuleError)> {
    let mut ids = Vec::new();
    ids.push(confine_id());
    for (index, text) in rules.iter().enumerate() {
        let rule = parse(text.as_ref()).map_err(|error| (index, error))?;
        let kind = match rule.grant {
            Grant::Read => Kind::Read,
            Grant::Write => Kind::Write,
        };
        ids.push(rule_id(kind, &rule.subject()));
        for ancestor in rule.ancestor_subjects() {
            ids.push(rule_id(Kind::Trav, &ancestor));
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}
