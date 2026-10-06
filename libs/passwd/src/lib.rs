//! The account file, `/system/etc/passwd` (issue #508, docs/filesystem-plan.md
//! F4 section 3).
//!
//! One account per line, `name:uid:gid:x:home:shell`. The file is the
//! **only** account source: `accountsd` has no built-in table, so this parser
//! decides whether the machine has accounts at all. It is strict on purpose and
//! fails closed: a file that is empty, too large, not text, has a malformed row
//! or reuses a name or a uid yields a [`LoadError`] and no account, rather than
//! the rows that happened to parse. A half-read account file is how a
//! corrupted disk turns into an unexpected login.
//!
//! Blank lines and `#` comments are allowed; a trailing `\r` is ignored. The
//! fourth field must be exactly `x`: the passwords are Argon2id verifiers in
//! the root-only `/system/etc/shadow` ([`shadow`], issue #447), and a row that
//! carries anything else (a plaintext secret, say) is refused like any other
//! malformed row, so no secret can come back into the world-readable file.
//!
//! The image build parses its own copy with the same function, so a passwd
//! that `accountsd` would refuse never reaches an image.
#![no_std]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

pub mod shadow;
#[cfg(test)]
mod tests;

/// Largest account file accepted, in bytes.
pub const PASSWD_MAX: usize = 1024;
/// Longest account name.
pub const NAME_MAX: usize = 32;
/// Fields per row.
const FIELDS: usize = 6;

/// The password field of every row: the secret is in the shadow file.
pub const IN_SHADOW: &str = "x";

/// One account row (its password is in the shadow file, never here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub shell: String,
}

/// Why there are no accounts. [`fmt::Display`] is the `reason=` text of
/// `ACCOUNTS:LOAD:FAIL`: one word, then `key=value` details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The file does not exist.
    Missing,
    /// The file exists but could not be read (the errno).
    Unreadable(i64),
    /// Larger than [`PASSWD_MAX`].
    Oversize(usize),
    /// Not UTF-8.
    NotText,
    /// No account row (empty, or only blank lines and comments).
    NoValidRow,
    /// Row `line` (1-based) is malformed in `field`.
    BadRow { line: usize, field: &'static str },
    /// Two rows share a uid (the second is at `line`).
    DuplicateUid { line: usize, uid: u32 },
    /// Two rows share a name (the second is at `line`).
    DuplicateName { line: usize },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Missing => f.write_str("missing"),
            LoadError::Unreadable(errno) => write!(f, "unreadable errno={errno}"),
            LoadError::Oversize(size) => write!(f, "oversize bytes={size} max={PASSWD_MAX}"),
            LoadError::NotText => f.write_str("not-utf8"),
            LoadError::NoValidRow => f.write_str("no-valid-row"),
            LoadError::BadRow { line, field } => write!(f, "bad-row line={line} field={field}"),
            LoadError::DuplicateUid { line, uid } => {
                write!(f, "duplicate-uid line={line} uid={uid}")
            }
            LoadError::DuplicateName { line } => write!(f, "duplicate-name line={line}"),
        }
    }
}

/// Parse a whole account file. Every row must be valid and unique, and there
/// must be at least one; anything else is a [`LoadError`].
pub fn parse(bytes: &[u8]) -> Result<Vec<Entry>, LoadError> {
    if bytes.len() > PASSWD_MAX {
        return Err(LoadError::Oversize(bytes.len()));
    }
    let text = core::str::from_utf8(bytes).map_err(|_| LoadError::NotText)?;
    let mut entries: Vec<Entry> = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let line = index + 1;
        let row = raw.strip_suffix('\r').unwrap_or(raw);
        if row.trim().is_empty() || row.starts_with('#') {
            continue;
        }
        let entry = parse_row(row).map_err(|field| LoadError::BadRow { line, field })?;
        if entries.iter().any(|seen| seen.name == entry.name) {
            return Err(LoadError::DuplicateName { line });
        }
        if entries.iter().any(|seen| seen.uid == entry.uid) {
            return Err(LoadError::DuplicateUid {
                line,
                uid: entry.uid,
            });
        }
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err(LoadError::NoValidRow);
    }
    Ok(entries)
}

/// One `name:uid:gid:x:home:shell` row, or the name of the first field
/// that is wrong.
fn parse_row(row: &str) -> Result<Entry, &'static str> {
    let fields: Vec<&str> = row.split(':').collect();
    if fields.len() != FIELDS {
        return Err("count");
    }
    let [name, uid, gid, secret, home, shell] = [
        fields[0], fields[1], fields[2], fields[3], fields[4], fields[5],
    ];
    if !valid_name(name) {
        return Err("name");
    }
    let uid = parse_id(uid).ok_or("uid")?;
    let gid = parse_id(gid).ok_or("gid")?;
    if secret != IN_SHADOW {
        return Err("secret");
    }
    if !valid_home(home) {
        return Err("home");
    }
    if shell.is_empty() || shell.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("shell");
    }
    Ok(Entry {
        name: name.to_string(),
        uid,
        gid,
        home: home.to_string(),
        shell: shell.to_string(),
    })
}

/// A login name: `[a-z_][a-z0-9_-]*`, at most [`NAME_MAX`] bytes. It becomes a
/// path component (`/home/<name>`) and an environment value, so nothing else.
pub fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    name.len() <= NAME_MAX
        && (first.is_ascii_lowercase() || first == b'_')
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A decimal id that fits `u32`: digits only (no sign, no spaces).
fn parse_id(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// An absolute, normalised home path: no empty, `.` or `..` component.
fn valid_home(home: &str) -> bool {
    let Some(rest) = home.strip_prefix('/') else {
        return false;
    };
    !home.chars().any(char::is_control)
        && (rest.is_empty()
            || rest
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."))
}
