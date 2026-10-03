//! The journal line format.
//!
//! One record is one text line, five tab-separated fields:
//!
//! ```text
//! <seq>\t<tick>\t<topic>\t<detail>\t<hash>\n
//! ```
//!
//! `topic` and `detail` are escaped (`\\`, `\t`, `\n`, `\r`), so a field can
//! never split a line or a column, and `hash` is 16 lowercase hex digits. Each
//! boot starts every file it writes with a `boot` line (`seq` 0, topic
//! `boot`, detail `id=<boot id>`); every following line's hash chains over
//! the previous line's, FNV-1a over `(previous, seq, tick, topic, detail)`,
//! the same function the in-memory ring uses. A file started by a rotation
//! continues the chain: its boot line carries `cont=<hash>`, the last hash of
//! the file it replaced, and chains over it.

use alloc::format;
use alloc::string::String;

/// The topic of a boot line.
pub const BOOT_TOPIC: &str = "boot";
/// Longest line written (newline included); a longer detail is truncated
/// and marked with [`TRUNCATED`] so one record cannot take a whole file.
pub const MAX_LINE: usize = 2048;
/// Appended to a truncated detail.
pub const TRUNCATED: &str = "...";

/// FNV-1a over the previous hash and the record fields (`0` = genesis).
pub fn record_hash(previous: u64, seq: u64, tick: u64, topic: &str, detail: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = if previous == 0 { OFFSET } else { previous };
    for byte in seq
        .to_le_bytes()
        .iter()
        .chain(tick.to_le_bytes().iter())
        .chain(topic.as_bytes())
        .chain(b"|")
        .chain(detail.as_bytes())
    {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// The escaped form of one character, or `None` when it stands for itself.
fn escape_char(c: char) -> Option<&'static str> {
    match c {
        '\\' => Some("\\\\"),
        '\t' => Some("\\t"),
        '\n' => Some("\\n"),
        '\r' => Some("\\r"),
        _ => None,
    }
}

/// Append `text` to `out` escaped.
pub fn escape_into(text: &str, out: &mut String) {
    for c in text.chars() {
        match escape_char(c) {
            Some(escaped) => out.push_str(escaped),
            None => out.push(c),
        }
    }
}

/// `text` escaped.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(text, &mut out);
    out
}

/// Undo [`escape`]; `None` for a dangling or unknown escape.
pub fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next()? {
            '\\' => '\\',
            't' => '\t',
            'n' => '\n',
            'r' => '\r',
            _ => return None,
        });
    }
    Some(out)
}

/// The longest prefix of `detail` whose escaped form fits `room` bytes, with
/// [`TRUNCATED`] appended when anything was cut. The result is what is
/// hashed and stored, so a truncated record still verifies.
pub fn clip(detail: &str, room: usize) -> String {
    let full: usize = detail
        .chars()
        .map(|c| escape_char(c).map_or(c.len_utf8(), str::len))
        .sum();
    if full <= room {
        return String::from(detail);
    }
    let room = room.saturating_sub(TRUNCATED.len());
    let mut used = 0;
    let mut out = String::new();
    for c in detail.chars() {
        let width = escape_char(c).map_or(c.len_utf8(), str::len);
        if used + width > room {
            break;
        }
        used += width;
        out.push(c);
    }
    out.push_str(TRUNCATED);
    out
}

/// One record line, newline included. The caller has [`clip`]ped `detail`.
pub fn format_line(seq: u64, tick: u64, topic: &str, detail: &str, hash: u64) -> String {
    let mut line = format!("{seq}\t{tick}\t");
    escape_into(topic, &mut line);
    line.push('\t');
    escape_into(detail, &mut line);
    line.push_str(&format!("\t{hash:016x}\n"));
    line
}

/// The `boot` line's detail: the boot id, and the hash continued from a
/// rotated file (`0` for none).
pub fn boot_detail(boot_id: u64, cont: u64) -> String {
    if cont == 0 {
        format!("id={boot_id:016x}")
    } else {
        format!("id={boot_id:016x} cont={cont:016x}")
    }
}

/// One parsed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub seq: u64,
    pub tick: u64,
    pub topic: String,
    pub detail: String,
    pub hash: u64,
}

impl Line {
    /// The chain head a boot line starts from: its `cont=` value, else `0`.
    pub fn continued(&self) -> u64 {
        self.detail
            .split(' ')
            .find_map(|field| field.strip_prefix("cont="))
            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
            .unwrap_or(0)
    }

    pub fn is_boot(&self) -> bool {
        self.seq == 0 && self.topic == BOOT_TOPIC
    }
}

/// Parse one line (without its newline); `None` when malformed.
pub fn parse_line(text: &str) -> Option<Line> {
    let mut fields = text.split('\t');
    let line = Line {
        seq: fields.next()?.parse().ok()?,
        tick: fields.next()?.parse().ok()?,
        topic: unescape(fields.next()?)?,
        detail: unescape(fields.next()?)?,
        hash: u64::from_str_radix(fields.next()?, 16).ok()?,
    };
    if fields.next().is_some() {
        return None;
    }
    Some(line)
}

/// What [`verify`] found wrong, by 0-based line index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Broken {
    /// A line that does not parse.
    Malformed(usize),
    /// A record before any boot line.
    NoBoot(usize),
    /// A hash that does not chain.
    Chain(usize),
}

/// Check a whole journal: every line parses, the file starts with a boot
/// line, and every line chains. Returns the number of records (boot lines
/// excluded).
///
/// A boot that finds a file it did not start writes a newline before its
/// boot line, so the empty line separating boots is skipped, and a line a
/// crash cut short (no newline) is closed by it: a malformed line directly
/// before a boot line is that torn tail and is tolerated. A final line
/// without a newline is a write in progress and is ignored.
pub fn verify(text: &str) -> Result<u64, Broken> {
    let complete = match text.rfind('\n') {
        Some(end) => &text[..end],
        None => return Ok(0),
    };
    let mut head: Option<u64> = None;
    let mut records = 0;
    let mut torn: Option<usize> = None;
    for (index, raw) in complete.split('\n').enumerate() {
        if raw.is_empty() {
            continue;
        }
        let Some(line) = parse_line(raw) else {
            if let Some(previous) = torn {
                return Err(Broken::Malformed(previous));
            }
            torn = Some(index);
            continue;
        };
        if let Some(previous) = torn.take() {
            if !line.is_boot() {
                return Err(Broken::Malformed(previous));
            }
        }
        let previous = if line.is_boot() {
            line.continued()
        } else {
            records += 1;
            head.ok_or(Broken::NoBoot(index))?
        };
        if record_hash(previous, line.seq, line.tick, &line.topic, &line.detail) != line.hash {
            return Err(Broken::Chain(index));
        }
        head = Some(line.hash);
    }
    match torn {
        Some(index) => Err(Broken::Malformed(index)),
        None => Ok(records),
    }
}
