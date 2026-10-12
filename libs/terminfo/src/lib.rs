//! A small terminfo toolchain: parse the source form of one entry, write the
//! legacy compiled format (what ncurses reads from `$TERMINFO_DIRS`), and read
//! a compiled entry back.
//!
//! The image build compiles the Terminal's `xterm-256color` entry with this, so
//! the result is reproducible and no `tic` is needed on the build host. Only
//! the predefined capabilities are supported (no extended section, no `use=`),
//! which is all the Terminal's entry needs; an unknown name is an error, not a
//! silent skip.

mod caps;

use std::collections::BTreeMap;

pub use caps::{BOOLS, NUMBERS, STRINGS};

/// The legacy format's magic number (octal 0432).
const MAGIC: i16 = 0x011a;
/// The largest number the legacy (16-bit) format holds.
const MAX_NUMBER: i32 = 32767;
/// A name longer than this (ncurses' limit) is refused.
const MAX_NAMES: usize = 128;

/// One terminal description: its names and the capabilities it sets, by index
/// into the predefined tables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// `xterm-256color|xterm with 256 colors`: names and the description.
    pub names: String,
    pub bools: BTreeMap<usize, bool>,
    pub numbers: BTreeMap<usize, i16>,
    pub strings: BTreeMap<usize, Vec<u8>>,
}

impl Entry {
    pub fn flag(&self, name: &str) -> bool {
        index(&BOOLS, name).is_some_and(|i| self.bools.get(&i).copied().unwrap_or(false))
    }

    pub fn number(&self, name: &str) -> Option<i16> {
        index(&NUMBERS, name).and_then(|i| self.numbers.get(&i).copied())
    }

    pub fn string(&self, name: &str) -> Option<&[u8]> {
        index(&STRINGS, name).and_then(|i| self.strings.get(&i).map(Vec::as_slice))
    }
}

fn index(table: &[&str], name: &str) -> Option<usize> {
    table.iter().position(|candidate| *candidate == name)
}

/// The value of a string capability's text: `\E`, `\n`, `\r`, `\t`, `\b`,
/// `\f`, `\s`, `\^`, `\\`, `\,`, `\:`, `\ddd` (octal), `^X` control
/// characters. Parameter operators (`%p1%d`) pass through untouched.
fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let b = bytes[at];
        at += 1;
        match b {
            b'\\' => {
                let Some(&escaped) = bytes.get(at) else {
                    return Err("a backslash ends the capability".into());
                };
                at += 1;
                out.push(match escaped {
                    b'E' | b'e' => 0x1b,
                    b'n' | b'l' => b'\n',
                    b'r' => b'\r',
                    b't' => b'\t',
                    b'b' => 0x08,
                    b'f' => 0x0c,
                    b's' => b' ',
                    b'0'..=b'7' => {
                        let mut value = u32::from(escaped - b'0');
                        for _ in 0..2 {
                            match bytes.get(at) {
                                Some(d @ b'0'..=b'7') => {
                                    value = value * 8 + u32::from(d - b'0');
                                    at += 1;
                                }
                                _ => break,
                            }
                        }
                        if value == 0 || value > 255 {
                            return Err(format!("octal escape {value:o} is out of range"));
                        }
                        value as u8
                    }
                    b'^' | b'\\' | b',' | b':' | b'\'' | b'"' => escaped,
                    other => return Err(format!("unknown escape \\{}", other as char)),
                });
            }
            b'^' => {
                let Some(&c) = bytes.get(at) else {
                    return Err("a caret ends the capability".into());
                };
                at += 1;
                out.push(if c == b'?' { 0x7f } else { c & 0x1f });
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

/// Split the capability list at commas that are not escaped.
fn split_caps(body: &str) -> Vec<String> {
    let mut caps = Vec::new();
    let mut current = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                current.push(c);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ',' => {
                let cap = current.trim();
                if !cap.is_empty() {
                    caps.push(cap.to_string());
                }
                current.clear();
            }
            other => current.push(other),
        }
    }
    let rest = current.trim();
    if !rest.is_empty() {
        caps.push(rest.to_string());
    }
    caps
}

/// Parse the source of one entry: a names line, then capabilities separated by
/// commas over any number of indented lines; `#` lines are comments.
pub fn parse_source(source: &str) -> Result<Entry, String> {
    let mut entry = Entry::default();
    let mut body = String::new();
    for line in source.lines() {
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            body.push_str(line.trim());
            body.push(' ');
        } else if entry.names.is_empty() {
            // The names line ends in a comma.
            entry.names = line.trim().trim_end_matches(',').to_string();
        } else {
            return Err(format!(
                "a second entry starts at {line:?}: one entry per file"
            ));
        }
    }
    if entry.names.is_empty() || entry.names.len() > MAX_NAMES || entry.names.contains('\0') {
        return Err("the entry has no usable names".into());
    }
    for cap in split_caps(&body) {
        if let Some((name, value)) = cap.split_once('#') {
            let at = index(&NUMBERS, name).ok_or_else(|| format!("unknown number {name:?}"))?;
            let number = if let Some(hex) = value.strip_prefix("0x") {
                i32::from_str_radix(hex, 16)
            } else {
                value.parse::<i32>()
            }
            .map_err(|_| format!("{name}: {value:?} is not a number"))?;
            if !(0..=MAX_NUMBER).contains(&number) {
                return Err(format!("{name}#{number} does not fit the legacy format"));
            }
            entry.numbers.insert(at, number as i16);
        } else if let Some((name, value)) = cap.split_once('=') {
            let at = index(&STRINGS, name).ok_or_else(|| format!("unknown string {name:?}"))?;
            let text = unescape(value).map_err(|e| format!("{name}: {e}"))?;
            entry.strings.insert(at, text);
        } else if let Some(name) = cap.strip_suffix('@') {
            // A cancelled capability is simply not set.
            let _ = name;
        } else {
            let at = index(&BOOLS, &cap).ok_or_else(|| format!("unknown boolean {cap:?}"))?;
            entry.bools.insert(at, true);
        }
    }
    Ok(entry)
}

fn put_short(out: &mut Vec<u8>, value: i16) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// The legacy compiled form of `entry`: header, names, booleans (padded to an
/// even offset), numbers, string offsets and the string table. Only the
/// capabilities up to the last one set are counted.
pub fn compile(entry: &Entry) -> Vec<u8> {
    let count = |set: Vec<usize>| set.into_iter().max().map_or(0, |last| last + 1);
    let bool_count = count(entry.bools.keys().copied().collect());
    let number_count = count(entry.numbers.keys().copied().collect());
    let string_count = count(entry.strings.keys().copied().collect());
    let mut table: Vec<u8> = Vec::new();
    let mut offsets = vec![-1i16; string_count];
    for (&at, text) in &entry.strings {
        offsets[at] = table.len() as i16;
        table.extend_from_slice(text);
        table.push(0);
    }
    let names = {
        let mut names = entry.names.clone().into_bytes();
        names.push(0);
        names
    };
    let mut out = Vec::new();
    for value in [
        MAGIC,
        names.len() as i16,
        bool_count as i16,
        number_count as i16,
        string_count as i16,
        table.len() as i16,
    ] {
        put_short(&mut out, value);
    }
    out.extend_from_slice(&names);
    for at in 0..bool_count {
        out.push(u8::from(entry.bools.get(&at).copied().unwrap_or(false)));
    }
    if (names.len() + bool_count) % 2 == 1 {
        out.push(0);
    }
    for at in 0..number_count {
        put_short(&mut out, entry.numbers.get(&at).copied().unwrap_or(-1));
    }
    for offset in offsets {
        put_short(&mut out, offset);
    }
    out.extend_from_slice(&table);
    out
}

/// Parse source and compile it.
pub fn compile_source(source: &str) -> Result<Vec<u8>, String> {
    parse_source(source).map(|entry| compile(&entry))
}

fn short(bytes: &[u8], at: usize) -> Result<i16, String> {
    bytes
        .get(at..at + 2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .ok_or_else(|| "the entry is truncated".to_string())
}

/// Read a legacy compiled entry (as ncurses does): used by the tests to prove
/// the writer's output, and by tools that want to look inside one.
pub fn read_compiled(bytes: &[u8]) -> Result<Entry, String> {
    if short(bytes, 0)? != MAGIC {
        return Err("not a legacy compiled terminfo entry".into());
    }
    let size =
        |at: usize| usize::try_from(short(bytes, at)?).map_err(|_| "negative size".to_string());
    let (names_len, bools, numbers, strings, table_len) =
        (size(2)?, size(4)?, size(6)?, size(8)?, size(10)?);
    let mut at = 12;
    let names = bytes
        .get(at..at + names_len)
        .ok_or("the names are truncated")?;
    let names = names
        .strip_suffix(&[0])
        .ok_or("the names are not terminated")?;
    let mut entry = Entry {
        names: String::from_utf8(names.to_vec()).map_err(|_| "the names are not text")?,
        ..Entry::default()
    };
    at += names_len;
    for i in 0..bools {
        let value = *bytes.get(at + i).ok_or("the booleans are truncated")?;
        if value == 1 {
            entry.bools.insert(i, true);
        }
    }
    at += bools;
    if (names_len + bools) % 2 == 1 {
        at += 1;
    }
    for i in 0..numbers {
        let value = short(bytes, at + 2 * i)?;
        if value >= 0 {
            entry.numbers.insert(i, value);
        }
    }
    at += 2 * numbers;
    let offsets: Vec<i16> = (0..strings)
        .map(|i| short(bytes, at + 2 * i))
        .collect::<Result<_, _>>()?;
    at += 2 * strings;
    let table = bytes
        .get(at..at + table_len)
        .ok_or("the string table is truncated")?;
    for (i, offset) in offsets.into_iter().enumerate() {
        if offset < 0 {
            continue;
        }
        let rest = table
            .get(offset as usize..)
            .ok_or("a string offset is out of range")?;
        let end = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or("a string is not terminated")?;
        entry.strings.insert(i, rest[..end].to_vec());
    }
    Ok(entry)
}

#[cfg(test)]
mod tests;
