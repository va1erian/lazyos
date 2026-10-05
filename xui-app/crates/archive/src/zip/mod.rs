//! Zip: the central-directory reader (zip64, UTF-8 and CP437 names, Unix
//! modes and extended timestamps), member data with CRC checking, and the
//! [`write`]r.
//!
//! The central directory is the index: it is read once at open, bounded by
//! the file's own size, and each member's data is found through its local
//! header when wanted. Offsets are checked against the file length before
//! any seek, and a member's data reader is limited to its compressed size.

mod cp437;
pub mod write;

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};

use flate2::read::DeflateDecoder;

use crate::entry::{Entry, EntryKind};
use crate::error::{Error, Result};

const LOCAL_SIG: u32 = 0x0403_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
/// The EOCD record without its comment.
const EOCD_LEN: u64 = 22;
/// The furthest an EOCD can sit from the end (a 64 KiB comment).
const EOCD_SEARCH: u64 = EOCD_LEN + 0xffff;

/// Flag bit 0: the member is encrypted.
const FLAG_ENCRYPTED: u16 = 1;
/// Flag bit 3: sizes and CRC follow the data in a descriptor.
pub(crate) const FLAG_DESCRIPTOR: u16 = 1 << 3;
/// Flag bit 11: the name is UTF-8.
pub(crate) const FLAG_UTF8: u16 = 1 << 11;

/// Unix file-type bits in the external attributes' high half.
const S_IFMT: u32 = 0o170_000;
const S_IFDIR: u32 = 0o040_000;
const S_IFLNK: u32 = 0o120_000;

/// What the central directory says about one member, beyond its [`Entry`]:
/// enough to read its data or copy it raw into a rewritten archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub method: u16,
    pub flags: u16,
    pub crc: u32,
    pub compressed: u64,
    pub uncompressed: u64,
    pub local_offset: u64,
    pub version_made_by: u16,
    pub external_attrs: u32,
    pub dos_time: u16,
    pub dos_date: u16,
    /// The name exactly as stored (kept for a raw copy).
    pub raw_name: Vec<u8>,
}

/// An open zip archive: its entries and how to reach each one's data.
pub struct ZipArchive {
    pub entries: Vec<Entry>,
    pub members: Vec<Member>,
    /// Bytes before the first member (a self-extractor's stub), added to
    /// every stored offset.
    base: u64,
    len: u64,
}

fn u16_at(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([buf[at], buf[at + 1]])
}

fn u32_at(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(buf[at..at + 4].try_into().unwrap_or([0; 4]))
}

fn u64_at(buf: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(buf[at..at + 8].try_into().unwrap_or([0; 8]))
}

/// The method's display name.
pub fn method_name(method: u16) -> String {
    match method {
        0 => "Store".into(),
        8 => "Deflate".into(),
        9 => "Deflate64".into(),
        12 => "BZip2".into(),
        14 => "LZMA".into(),
        93 => "Zstandard".into(),
        95 => "XZ".into(),
        99 => "AES".into(),
        other => format!("Method {other}"),
    }
}

impl ZipArchive {
    /// Read the central directory of `file`.
    pub fn open(file: &mut File) -> Result<ZipArchive> {
        let len = file.seek(SeekFrom::End(0))?;
        let (eocd_at, eocd) = find_eocd(file, len)?;
        let mut count = u64::from(u16_at(&eocd, 10));
        let mut cd_size = u64::from(u32_at(&eocd, 12));
        let mut cd_offset = u64::from(u32_at(&eocd, 16));
        let mut cd_end = eocd_at;
        if count == 0xffff || cd_size == 0xffff_ffff || cd_offset == 0xffff_ffff {
            if let Some((zip64_at, record)) = read_zip64_eocd(file, eocd_at)? {
                count = u64_at(&record, 32);
                cd_size = u64_at(&record, 40);
                cd_offset = u64_at(&record, 48);
                cd_end = zip64_at;
            }
        }
        // A stub before the archive shifts every offset by the same amount.
        let base = cd_end
            .checked_sub(cd_size)
            .and_then(|start| start.checked_sub(cd_offset))
            .ok_or_else(|| Error::corrupt("zip: central directory out of range"))?;
        if cd_size > len {
            return Err(Error::corrupt(
                "zip: central directory larger than the file",
            ));
        }
        file.seek(SeekFrom::Start(base + cd_offset))?;
        let mut cd = Vec::new();
        (&mut *file).take(cd_size).read_to_end(&mut cd)?;
        if cd.len() as u64 != cd_size {
            return Err(Error::corrupt("zip: truncated central directory"));
        }
        let (entries, members) = parse_central(&cd, count)?;
        let archive = ZipArchive {
            entries,
            members,
            base,
            len,
        };
        Ok(archive)
    }

    /// The absolute offset of member `index`'s data, after its local header.
    pub fn data_offset(&self, file: &mut File, index: usize) -> Result<u64> {
        let member = &self.members[index];
        let at = self
            .base
            .checked_add(member.local_offset)
            .filter(|&at| at.saturating_add(30) <= self.len)
            .ok_or_else(|| Error::corrupt("zip: member offset out of range"))?;
        file.seek(SeekFrom::Start(at))?;
        let mut header = [0u8; 30];
        file.read_exact(&mut header)?;
        if u32_at(&header, 0) != LOCAL_SIG {
            return Err(Error::corrupt("zip: bad local header"));
        }
        let skip = u64::from(u16_at(&header, 26)) + u64::from(u16_at(&header, 28));
        let start = at + 30 + skip;
        if start.saturating_add(member.compressed) > self.len {
            return Err(Error::corrupt("zip: member data past the end of the file"));
        }
        Ok(start)
    }

    /// A reader over member `index`'s decompressed data, checked against its
    /// CRC and size at the end. `file` is a second handle on the archive.
    pub fn reader(&self, mut file: File, index: usize) -> Result<Box<dyn Read + Send>> {
        let member = self.members[index].clone();
        if member.flags & FLAG_ENCRYPTED != 0 {
            return Err(Error::unsupported("encrypted zip members"));
        }
        let start = self.data_offset(&mut file, index)?;
        file.seek(SeekFrom::Start(start))?;
        let raw = BufReader::new(file).take(member.compressed);
        let data: Box<dyn Read + Send> = match member.method {
            0 => Box::new(raw),
            8 => Box::new(DeflateDecoder::new(raw)),
            other => return Err(Error::unsupported(format!("zip {}", method_name(other)))),
        };
        Ok(Box::new(Checked {
            inner: data,
            hasher: crc32fast::Hasher::new(),
            crc: member.crc,
            remaining: member.uncompressed,
            verified: false,
        }))
    }
}

/// Find the end-of-central-directory record, scanning back over a comment.
fn find_eocd(file: &mut File, len: u64) -> Result<(u64, Vec<u8>)> {
    let search = len.min(EOCD_SEARCH);
    file.seek(SeekFrom::Start(len - search))?;
    let mut tail = Vec::new();
    (&mut *file).take(search).read_to_end(&mut tail)?;
    let found = (0..tail.len().saturating_sub(EOCD_LEN as usize - 1))
        .rev()
        .find(|&i| u32_at(&tail, i) == EOCD_SIG);
    match found {
        Some(i) => Ok((
            len - search + i as u64,
            tail[i..i + EOCD_LEN as usize].to_vec(),
        )),
        None => Err(Error::corrupt("zip: no end of central directory")),
    }
}

/// The zip64 end record a zip64 locator before the EOCD points at.
fn read_zip64_eocd(file: &mut File, eocd_at: u64) -> Result<Option<(u64, [u8; 56])>> {
    let Some(locator_at) = eocd_at.checked_sub(20) else {
        return Ok(None);
    };
    file.seek(SeekFrom::Start(locator_at))?;
    let mut locator = [0u8; 20];
    file.read_exact(&mut locator)?;
    if u32_at(&locator, 0) != ZIP64_LOCATOR_SIG {
        return Ok(None);
    }
    let record_at = u64_at(&locator, 8);
    if record_at >= locator_at {
        return Err(Error::corrupt("zip: zip64 record out of range"));
    }
    file.seek(SeekFrom::Start(record_at))?;
    let mut record = [0u8; 56];
    file.read_exact(&mut record)?;
    if u32_at(&record, 0) != ZIP64_EOCD_SIG {
        return Err(Error::corrupt("zip: bad zip64 end record"));
    }
    Ok(Some((record_at, record)))
}

/// Parse the central headers filling `cd`. The stated `count` sizes
/// nothing: the directory's own bytes bound the loop.
fn parse_central(cd: &[u8], count: u64) -> Result<(Vec<Entry>, Vec<Member>)> {
    let mut entries = Vec::new();
    let mut members = Vec::new();
    let mut at = 0usize;
    while at < cd.len() {
        if at + 46 > cd.len() {
            return Err(Error::corrupt("zip: truncated central header"));
        }
        let h = &cd[at..];
        if u32_at(h, 0) != CENTRAL_SIG {
            return Err(Error::corrupt("zip: bad central header"));
        }
        let name_len = usize::from(u16_at(h, 28));
        let extra_len = usize::from(u16_at(h, 30));
        let comment_len = usize::from(u16_at(h, 32));
        let total = 46 + name_len + extra_len + comment_len;
        if at + total > cd.len() {
            return Err(Error::corrupt("zip: central header past the directory"));
        }
        let raw_name = h[46..46 + name_len].to_vec();
        let extra = &h[46 + name_len..46 + name_len + extra_len];
        let mut member = Member {
            version_made_by: u16_at(h, 4),
            flags: u16_at(h, 8),
            method: u16_at(h, 10),
            dos_time: u16_at(h, 12),
            dos_date: u16_at(h, 14),
            crc: u32_at(h, 16),
            compressed: u64::from(u32_at(h, 20)),
            uncompressed: u64::from(u32_at(h, 24)),
            external_attrs: u32_at(h, 38),
            local_offset: u64::from(u32_at(h, 42)),
            raw_name,
        };
        let mtime = apply_extra(extra, &mut member);
        entries.push(member_entry(entries.len(), &member, mtime));
        members.push(member);
        at += total;
    }
    // A saturated 16-bit count (65535) is only a lower bound.
    if count != 0xffff && entries.len() as u64 != count {
        return Err(Error::corrupt(
            "zip: entry count does not match the directory",
        ));
    }
    Ok((entries, members))
}

/// Apply the zip64 and extended-timestamp extra fields; returns the
/// timestamp's mtime when there is one.
fn apply_extra(extra: &[u8], member: &mut Member) -> Option<i64> {
    let mut mtime = None;
    let mut at = 0;
    while at + 4 <= extra.len() {
        let id = u16_at(extra, at);
        let size = usize::from(u16_at(extra, at + 2));
        let Some(body) = extra.get(at + 4..at + 4 + size) else {
            break;
        };
        match id {
            0x0001 => {
                // Only the fields whose 32-bit value is saturated are present.
                let mut field = 0;
                let mut next = |slot: &mut u64| {
                    if *slot == 0xffff_ffff && field + 8 <= body.len() {
                        *slot = u64_at(body, field);
                        field += 8;
                    }
                };
                next(&mut member.uncompressed);
                next(&mut member.compressed);
                next(&mut member.local_offset);
            }
            0x5455 if !body.is_empty() && body[0] & 1 != 0 && body.len() >= 5 => {
                mtime = Some(i64::from(u32_at(body, 1) as i32));
            }
            _ => {}
        }
        at += 4 + size;
    }
    mtime
}

/// The [`Entry`] a central header describes.
fn member_entry(index: usize, member: &Member, mtime: Option<i64>) -> Entry {
    let name = if member.flags & FLAG_UTF8 != 0 {
        String::from_utf8_lossy(&member.raw_name).into_owned()
    } else {
        cp437::decode(&member.raw_name)
    };
    let unix = member.version_made_by >> 8 == 3;
    let unix_mode = (member.external_attrs >> 16) & 0xffff;
    let kind = if name.ends_with('/')
        || name.ends_with('\\')
        || member.external_attrs & 0x10 != 0
        || (unix && unix_mode & S_IFMT == S_IFDIR)
    {
        EntryKind::Dir
    } else if unix && unix_mode & S_IFMT == S_IFLNK {
        // The target is the member's data; the reader fills it in.
        EntryKind::Symlink {
            target: String::new(),
        }
    } else {
        EntryKind::File
    };
    let mut entry = Entry::new(index, &name, kind);
    if matches!(entry.kind, EntryKind::File) {
        entry.size = member.uncompressed;
    }
    entry.packed = Some(member.compressed);
    entry.method = method_name(member.method);
    entry.encrypted = member.flags & FLAG_ENCRYPTED != 0;
    entry.crc = Some(member.crc);
    entry.mode = (unix && unix_mode != 0).then_some(unix_mode & 0o7777);
    entry.modified = mtime.or_else(|| crate::time::from_dos(member.dos_date, member.dos_time));
    entry
}

/// Decompressed member data that fails at its end when the CRC or the size
/// does not match the central directory.
struct Checked {
    inner: Box<dyn Read + Send>,
    hasher: crc32fast::Hasher,
    crc: u32,
    remaining: u64,
    verified: bool,
}

impl Read for Checked {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "zip: member larger than recorded",
            ));
        }
        self.remaining -= n as u64;
        self.hasher.update(&buf[..n]);
        if n == 0 && !buf.is_empty() && !self.verified {
            if self.remaining != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "zip: member shorter than recorded",
                ));
            }
            if self.hasher.clone().finalize() != self.crc {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "zip: CRC mismatch",
                ));
            }
            self.verified = true;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests;
