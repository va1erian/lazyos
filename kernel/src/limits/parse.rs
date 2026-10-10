//! Parsing the `limit.<key>=<value>` lines of `lazyos.cfg`.
//!
//! Pure (no I/O, no globals) so the suite can feed it hostile text. Lines that
//! do not start with `limit.` belong to `fs::bootcfg` and are skipped here,
//! just as `bootcfg` skips these. A value is a decimal integer, for byte-sized
//! keys optionally followed by one of `K`, `M`, `G`, `T` (binary multiples).
//! An inline `# comment` after the value is allowed.

use alloc::vec::Vec;

use super::{Id, COUNT, KEYS};

/// The prefix that routes a `lazyos.cfg` line to this module.
pub const PREFIX: &str = "limit.";

/// A value accepted for one limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Override {
    pub id: Id,
    /// The value after clamping into the key's range.
    pub value: u64,
    /// Whether clamping changed it.
    pub clamped: bool,
}

/// What became of one `limit.` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome<'a> {
    Set(Override),
    /// `limit.<key>` names no known limit.
    Unknown(&'a str),
    /// The value is not a number this key accepts (the default stays).
    Malformed(&'a str),
    /// The key appeared before; this later line replaces it.
    Duplicate(&'a str),
}

/// Every `limit.` line of `text`, in order. Never fails as a whole.
pub fn parse(text: &str) -> Vec<Outcome<'_>> {
    let mut out = Vec::new();
    let mut seen = [false; COUNT];
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(PREFIX) else {
            continue;
        };
        let Some((key, value)) = rest.split_once('=') else {
            out.push(Outcome::Malformed(rest.trim()));
            continue;
        };
        let key = key.trim();
        let value = value.split('#').next().unwrap_or("").trim();
        let Some(index) = KEYS.iter().position(|known| known.name == key) else {
            out.push(Outcome::Unknown(key));
            continue;
        };
        let spec = KEYS[index];
        let parsed = if spec.bytes {
            parse_size(value)
        } else {
            parse_count(value)
        };
        let Some(raw) = parsed else {
            out.push(Outcome::Malformed(key));
            continue;
        };
        if core::mem::replace(&mut seen[index], true) {
            out.push(Outcome::Duplicate(key));
        }
        let value = raw.clamp(spec.min, spec.max);
        out.push(Outcome::Set(Override {
            id: id_at(index),
            value,
            clamped: value != raw,
        }));
    }
    out
}

/// The [`Id`] at position `index` of [`KEYS`].
fn id_at(index: usize) -> Id {
    const IDS: [Id; COUNT] = [
        Id::HeapMax,
        Id::FdMax,
        Id::StackSize,
        Id::QuotaUserMemory,
        Id::QuotaKernelMemory,
        Id::SharedBufferMax,
        Id::ScratchMax,
    ];
    IDS[index]
}

/// A plain decimal count: digits only, no sign, no suffix, no overflow.
fn parse_count(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A byte size: decimal digits and an optional `K`/`M`/`G`/`T` suffix (either
/// case, binary multiples). `None` for anything else, including overflow.
pub fn parse_size(text: &str) -> Option<u64> {
    let (digits, shift) = match text.as_bytes().last()? {
        b'k' | b'K' => (&text[..text.len() - 1], 10),
        b'm' | b'M' => (&text[..text.len() - 1], 20),
        b'g' | b'G' => (&text[..text.len() - 1], 30),
        b't' | b'T' => (&text[..text.len() - 1], 40),
        _ => (text, 0),
    };
    parse_count(digits)?.checked_mul(1u64 << shift)
}
