//! Tar: a streaming reader for ustar, GNU (long names, base-256 sizes) and
//! pax (`path`, `linkpath`, `size`, `mtime`) members, and the [`write`]r.
//!
//! A tarball has no index, so listing and extraction both [`walk`] the
//! stream from the start. Metadata members (pax and GNU long-name headers)
//! are bounded ([`MAX_META`]) because their contents are read into memory;
//! file data is only ever streamed.

pub mod write;

use std::io::{self, Read};

use crate::entry::{Entry, EntryKind};
use crate::error::{Error, Result};

/// The tar block size.
pub const BLOCK: usize = 512;
/// Largest pax or GNU long-name member read into memory.
const MAX_META: u64 = 1024 * 1024;

/// What a [`walk`] callback asks for next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Go on to the next member.
    Continue,
    /// Stop reading (everything wanted has been seen).
    Stop,
}

/// Whether `block` is a tar header: not all zeros, and its checksum matches
/// (as the unsigned sum, or the signed one some old tars used).
pub fn is_header(block: &[u8]) -> bool {
    if block.len() < BLOCK || block.iter().all(|&b| b == 0) {
        return false;
    }
    let Some(stored) = parse_octal(&block[148..156]) else {
        return false;
    };
    let (unsigned, signed) = checksums(block);
    stored == unsigned || stored as i64 == signed
}

/// The header checksum with the checksum field read as spaces.
fn checksums(block: &[u8]) -> (u64, i64) {
    let mut unsigned = 0u64;
    let mut signed = 0i64;
    for (i, &byte) in block[..BLOCK].iter().enumerate() {
        let byte = if (148..156).contains(&i) { b' ' } else { byte };
        unsigned += u64::from(byte);
        signed += i64::from(byte as i8);
    }
    (unsigned, signed)
}

/// An octal field (spaces and NULs around the digits), or a GNU base-256
/// number when the high bit of the first byte is set.
fn parse_number(field: &[u8]) -> Option<u64> {
    match field.first() {
        Some(&first) if first & 0x80 != 0 => {
            // Base-256: the rest of the first byte, then big-endian bytes.
            if first & 0x40 != 0 {
                return None; // negative
            }
            let mut value = u64::from(first & 0x3f);
            for &byte in &field[1..] {
                value = value.checked_mul(256)?.checked_add(u64::from(byte))?;
            }
            Some(value)
        }
        _ => parse_octal(field),
    }
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let text: Vec<u8> = field
        .iter()
        .copied()
        .skip_while(|&b| b == b' ' || b == 0)
        .take_while(|&b| b != b' ' && b != 0)
        .collect();
    if text.is_empty() {
        return Some(0);
    }
    let mut value = 0u64;
    for byte in text {
        if !(b'0'..=b'7').contains(&byte) {
            return None;
        }
        value = value.checked_mul(8)?.checked_add(u64::from(byte - b'0'))?;
    }
    Some(value)
}

/// A NUL-terminated header string.
fn text(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// What earlier metadata members say about the next real member.
#[derive(Default)]
struct Pending {
    path: Option<String>,
    link: Option<String>,
    size: Option<u64>,
    mtime: Option<i64>,
}

/// Read exactly `buf.len()` bytes, or report a clean end (`Ok(false)`) when
/// the stream ends before the first byte.
fn read_block(input: &mut dyn Read, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(Error::corrupt("tar: truncated header")),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(true)
}

/// Discard `count` bytes of `input`, failing if it ends first.
fn skip(input: &mut dyn Read, count: u64) -> Result<()> {
    let copied = io::copy(&mut (&mut *input).take(count), &mut io::sink())?;
    if copied != count {
        return Err(Error::corrupt("tar: truncated member"));
    }
    Ok(())
}

/// The padding after `size` bytes of data.
fn padding(size: u64) -> u64 {
    (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64
}

/// Read a metadata member's whole (bounded) content.
fn read_meta(input: &mut dyn Read, size: u64) -> Result<Vec<u8>> {
    if size > MAX_META {
        return Err(Error::corrupt("tar: oversized metadata member"));
    }
    let mut data = Vec::new();
    (&mut *input).take(size).read_to_end(&mut data)?;
    if data.len() as u64 != size {
        return Err(Error::corrupt("tar: truncated metadata member"));
    }
    skip(input, padding(size))?;
    Ok(data)
}

/// Apply pax records (`<len> <key>=<value>\n`) to `pending`.
fn apply_pax(records: &[u8], pending: &mut Pending) {
    let mut rest = records;
    while !rest.is_empty() {
        let Some(space) = rest.iter().position(|&b| b == b' ') else {
            return;
        };
        let Some(len) = std::str::from_utf8(&rest[..space])
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            return;
        };
        if len <= space || len > rest.len() {
            return;
        }
        let record = &rest[space + 1..len];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(eq) = record.iter().position(|&b| b == b'=') {
            let value = String::from_utf8_lossy(&record[eq + 1..]).into_owned();
            match &record[..eq] {
                b"path" => pending.path = Some(value),
                b"linkpath" => pending.link = Some(value),
                b"size" => pending.size = value.parse().ok(),
                b"mtime" => {
                    pending.mtime = value.split('.').next().and_then(|secs| secs.parse().ok())
                }
                _ => {}
            }
        }
        rest = &rest[len..];
    }
}

/// Walk every member of the tar stream `input`, calling `visit` with its
/// [`Entry`] and a reader over exactly its data; whatever `visit` leaves
/// unread is skipped. Members are numbered from 0 in stream order, which is
/// the order [`crate::Archive`] lists them in.
pub fn walk(
    input: &mut dyn Read,
    visit: &mut dyn FnMut(Entry, &mut dyn Read) -> Result<Flow>,
) -> Result<()> {
    let mut header = [0u8; BLOCK];
    let mut pending = Pending::default();
    let mut index = 0;
    loop {
        if !read_block(input, &mut header)? || header.iter().all(|&b| b == 0) {
            return Ok(());
        }
        if !is_header(&header) {
            return Err(Error::corrupt("tar: bad header checksum"));
        }
        let size =
            parse_number(&header[124..136]).ok_or_else(|| Error::corrupt("tar: bad size field"))?;
        let flag = header[156];
        match flag {
            b'x' => {
                apply_pax(&read_meta(input, size)?, &mut pending);
                continue;
            }
            b'L' => {
                pending.path = Some(text(&read_meta(input, size)?));
                continue;
            }
            b'K' => {
                pending.link = Some(text(&read_meta(input, size)?));
                continue;
            }
            // Global pax headers and GNU volume/sparse metadata: skipped.
            b'g' | b'V' | b'M' | b'N' => {
                skip(input, size + padding(size))?;
                continue;
            }
            _ => {}
        }
        let pending_now = std::mem::take(&mut pending);
        let size = pending_now.size.unwrap_or(size);
        let entry = member_entry(index, &header, flag, size, pending_now);
        index += 1;
        let data_size = if matches!(entry.kind, EntryKind::File) {
            size
        } else {
            0
        };
        // Non-file members may still carry data (a hard link's, a device's):
        // skip it too.
        let mut data = (&mut *input).take(data_size);
        let flow = visit(entry, &mut data)?;
        let unread = data.limit();
        skip(input, unread)?;
        skip(input, size - data_size + padding(size))?;
        if flow == Flow::Stop {
            return Ok(());
        }
    }
}

/// The [`Entry`] a real member's header describes.
fn member_entry(
    index: usize,
    header: &[u8; BLOCK],
    flag: u8,
    size: u64,
    pending: Pending,
) -> Entry {
    let ustar = &header[257..262] == b"ustar";
    let name = pending.path.unwrap_or_else(|| {
        let name = text(&header[..100]);
        let prefix = if ustar {
            text(&header[345..500])
        } else {
            String::new()
        };
        if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        }
    });
    let link = pending.link.unwrap_or_else(|| text(&header[157..257]));
    let kind = match flag {
        b'0' | 0 | b'7' if name.ends_with('/') => EntryKind::Dir,
        b'0' | 0 | b'7' => EntryKind::File,
        b'5' => EntryKind::Dir,
        b'2' => EntryKind::Symlink { target: link },
        b'1' => EntryKind::Hardlink {
            target: crate::entry::normalize(&link).0,
        },
        _ => EntryKind::Special,
    };
    let mut entry = Entry::new(index, &name, kind);
    if matches!(entry.kind, EntryKind::File) {
        entry.size = size;
    }
    entry.mode = parse_octal(&header[100..108]).map(|mode| (mode & 0o7777) as u32);
    entry.modified = pending
        .mtime
        .or_else(|| parse_number(&header[136..148]).and_then(|t| i64::try_from(t).ok()));
    entry
}

#[cfg(test)]
mod tests;
