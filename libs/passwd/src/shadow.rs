//! The password file, `/system/etc/shadow` (issue #447).
//!
//! One verifier per line, `name:argon2id:<m_kib>:<t>:<p>:<salt>:<hash>`: the
//! account name, the Argon2id cost (memory in KiB, passes, lanes) and the salt
//! and verifier in lowercase hex. The image build writes it (mode 0600, owned
//! by root) and only `keyd` reads it: the verifiers never leave `keyd`, and no
//! plaintext password is stored anywhere on the volume.
//!
//! Like the account file it is parsed strictly and fails closed: a row that
//! is malformed in any field, a repeated name, a file that is not text or too
//! large yields a [`LoadError`] and no verifier at all. A row for a name the
//! account file lacks is harmless (nobody can be looked up to log in as it);
//! an account without a row cannot log in.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::{valid_name, LoadError};

/// Largest shadow file accepted, in bytes.
pub const SHADOW_MAX: usize = 4096;
/// The only hash this file carries.
pub const ALGORITHM: &str = "argon2id";
/// Salt bytes accepted: at least Argon2's minimum, at most a generous bound.
pub const SALT_MIN: usize = 8;
pub const SALT_MAX: usize = 32;
/// Verifier bytes (Argon2id output length `keyd` compares).
pub const VERIFIER_LEN: usize = 32;
/// Fields per row.
const FIELDS: usize = 7;

/// The Argon2id cost a verifier was derived with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cost {
    /// Memory in KiB.
    pub m_kib: u32,
    /// Passes.
    pub t: u32,
    /// Lanes.
    pub p: u32,
}

/// One verifier row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub cost: Cost,
    pub salt: Vec<u8>,
    pub verifier: [u8; VERIFIER_LEN],
}

/// The text of one row, as the image build writes it.
pub fn format_row(name: &str, cost: Cost, salt: &[u8], verifier: &[u8; VERIFIER_LEN]) -> String {
    format!(
        "{name}:{ALGORITHM}:{}:{}:{}:{}:{}",
        cost.m_kib,
        cost.t,
        cost.p,
        hex(salt),
        hex(verifier)
    )
}

/// Parse a whole shadow file. Every row must be valid and its name unique,
/// and there must be at least one.
pub fn parse(bytes: &[u8]) -> Result<Vec<Entry>, LoadError> {
    if bytes.len() > SHADOW_MAX {
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
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err(LoadError::NoValidRow);
    }
    Ok(entries)
}

/// One row, or the name of the first field that is wrong. Public for the
/// account database (`libs/accountdb`), whose rows carry the same verifier.
pub fn parse_row(row: &str) -> Result<Entry, &'static str> {
    let fields: Vec<&str> = row.split(':').collect();
    if fields.len() != FIELDS {
        return Err("count");
    }
    if !valid_name(fields[0]) {
        return Err("name");
    }
    if fields[1] != ALGORITHM {
        return Err("algorithm");
    }
    let cost = Cost {
        m_kib: number(fields[2]).ok_or("m_kib")?,
        t: number(fields[3]).ok_or("t")?,
        p: number(fields[4]).ok_or("p")?,
    };
    let salt = unhex(fields[5])
        .filter(|salt| (SALT_MIN..=SALT_MAX).contains(&salt.len()))
        .ok_or("salt")?;
    let verifier: [u8; VERIFIER_LEN] = unhex(fields[6])
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("hash")?;
    Ok(Entry {
        name: fields[0].to_string(),
        cost,
        salt,
        verifier,
    })
}

/// A positive decimal `u32`: digits only.
fn number(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 10 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|value| *value > 0)
}

/// Lowercase hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0xf)] as char);
    }
    out
}

/// The bytes of a lowercase hex string; `None` for odd length or a stray
/// character (uppercase included: there is one spelling per value).
fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let digit = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    text.as_bytes()
        .chunks(2)
        .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const COST: Cost = Cost {
        m_kib: 1024,
        t: 3,
        p: 1,
    };

    fn row(name: &str) -> String {
        format_row(name, COST, &[0xab; 16], &[0x5a; VERIFIER_LEN])
    }

    #[test]
    fn a_formatted_row_parses_back() {
        let text = format!("# verifiers\n{}\r\n\n{}\n", row("admin"), row("user"));
        let entries = parse(text.as_bytes()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "admin");
        assert_eq!(entries[0].cost, COST);
        assert_eq!(entries[0].salt, [0xab; 16]);
        assert_eq!(entries[1].verifier, [0x5a; VERIFIER_LEN]);
    }

    #[test]
    fn every_field_is_checked() {
        let good = row("admin");
        let fields: Vec<&str> = good.split(':').collect();
        let with = |at: usize, value: &str| {
            let mut copy = fields.clone();
            copy[at] = value;
            copy.join(":")
        };
        let short_salt = "ab".repeat(SALT_MIN - 1);
        let long_salt = "ab".repeat(SALT_MAX + 1);
        let short_hash = "5a".repeat(VERIFIER_LEN - 1);
        let cases = [
            (with(0, "Admin"), "name"),
            (with(1, "plain"), "algorithm"),
            (with(2, "0"), "m_kib"),
            (with(3, "x"), "t"),
            (with(4, ""), "p"),
            (with(5, "zz"), "salt"),
            (with(5, &short_salt), "salt"),
            (with(5, &long_salt), "salt"),
            (with(5, &"AB".repeat(16)), "salt"),
            (with(6, &short_hash), "hash"),
            (with(6, "5"), "hash"),
            (format!("{good}:extra"), "count"),
            (String::from("admin:nimda"), "count"),
        ];
        for (text, field) in cases {
            assert_eq!(
                parse(text.as_bytes()),
                Err(LoadError::BadRow { line: 1, field }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn it_fails_closed() {
        assert_eq!(parse(b""), Err(LoadError::NoValidRow));
        let twice = format!("{}\n{}\n", row("user"), row("user"));
        assert_eq!(
            parse(twice.as_bytes()),
            Err(LoadError::DuplicateName { line: 2 })
        );
        assert_eq!(parse(b"\xff\n"), Err(LoadError::NotText));
        let big = alloc::vec![b'#'; SHADOW_MAX + 1];
        assert_eq!(parse(&big), Err(LoadError::Oversize(SHADOW_MAX + 1)));
        // One bad row costs every verifier.
        let mixed = format!("{}\nuser:argon2id:1:1:1:00:00\n", row("admin"));
        assert!(parse(mixed.as_bytes()).is_err());
    }
}
