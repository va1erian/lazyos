#![forbid(unsafe_code)]

//! A small zip container: a stored-only writer and a bounded reader.
//!
//! Why not `lazyarc`: its zip reads and writes through `std::fs::File` and the
//! crate carries lzma and zstd, while this crate does no I/O (`tests/no_io.rs`)
//! and holds the whole file in memory. An OpenRaster file needs only entries
//! written in a fixed order, uncompressed. The deflate and CRC-32 code is the
//! same `flate2` and `crc32fast` `lazyarc` uses, so the app links one copy.
//!
//! The reader works from the central directory, as every zip tool does, and
//! treats the file as hostile: offsets and sizes are checked against the
//! buffer, no entry may inflate past the caller's limit, and the CRC-32 must
//! match. Zip64, encryption and multi-disk archives are refused.

use std::io::Read;

use flate2::read::DeflateDecoder;

/// The CRC-32 (IEEE) of `data`.
fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// Inflates raw DEFLATE `data`, refusing output past `limit` bytes (a zip
/// bomb is cut off at the limit, never buffered whole).
pub(super) fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::with_capacity(limit.min(1 << 16));
    let cap = u64::try_from(limit).map_or(u64::MAX, |n| n.saturating_add(1));
    DeflateDecoder::new(data)
        .take(cap)
        .read_to_end(&mut out)
        .map_err(|_| "corrupt deflate data")?;
    if out.len() > limit {
        return Err("the entry inflates past its limit");
    }
    Ok(out)
}

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
/// The most entries read; an OpenRaster file has two per layer plus a few.
const MAX_ENTRIES: usize = 4096;
/// 1980-01-01, the DOS date every entry gets (the format has no "unset").
const DOS_DATE: u16 = 0x0021;

/// Builds a zip of stored (uncompressed) entries, in the order added.
#[derive(Default)]
pub struct ZipWriter {
    out: Vec<u8>,
    central: Vec<u8>,
    entries: usize,
}

impl ZipWriter {
    /// An empty archive.
    pub fn new() -> ZipWriter {
        ZipWriter::default()
    }

    /// Appends `data` as the stored entry `name`.
    pub fn add(&mut self, name: &str, data: &[u8]) -> Result<(), String> {
        let too_big = || "an archive entry is too large".to_string();
        let size = u32::try_from(data.len()).map_err(|_| too_big())?;
        let offset = u32::try_from(self.out.len()).map_err(|_| too_big())?;
        let name_len = u16::try_from(name.len()).map_err(|_| "entry name too long".to_string())?;
        let crc = crc32(data);
        // Version 10, flags 0 (names are UTF-8-compatible ASCII in practice;
        // bit 11 marks UTF-8 for the rest), method 0 = stored.
        let flags: u16 = 1 << 11;
        let mut local = Vec::with_capacity(30 + name.len());
        push32(&mut local, LOCAL);
        push16(&mut local, 10);
        push16(&mut local, flags);
        push16(&mut local, 0);
        push16(&mut local, 0);
        push16(&mut local, DOS_DATE);
        push32(&mut local, crc);
        push32(&mut local, size);
        push32(&mut local, size);
        push16(&mut local, name_len);
        push16(&mut local, 0);
        local.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(&local);
        self.out.extend_from_slice(data);

        let c = &mut self.central;
        push32(c, CENTRAL);
        push16(c, 20);
        push16(c, 10);
        push16(c, flags);
        push16(c, 0);
        push16(c, 0);
        push16(c, DOS_DATE);
        push32(c, crc);
        push32(c, size);
        push32(c, size);
        push16(c, name_len);
        push16(c, 0);
        push16(c, 0);
        push16(c, 0);
        push16(c, 0);
        push32(c, 0);
        push32(c, offset);
        c.extend_from_slice(name.as_bytes());
        self.entries += 1;
        Ok(())
    }

    /// Writes the central directory and returns the archive.
    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        let too_big = || "the archive is too large".to_string();
        let entries = u16::try_from(self.entries).map_err(|_| too_big())?;
        let start = u32::try_from(self.out.len()).map_err(|_| too_big())?;
        let size = u32::try_from(self.central.len()).map_err(|_| too_big())?;
        self.out.extend_from_slice(&self.central);
        u32::try_from(self.out.len()).map_err(|_| too_big())?;
        let out = &mut self.out;
        push32(out, END);
        push16(out, 0);
        push16(out, 0);
        push16(out, entries);
        push16(out, entries);
        push32(out, size);
        push32(out, start);
        push16(out, 0);
        Ok(self.out)
    }
}

fn push16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn push32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// One entry of the central directory.
struct Entry {
    name: String,
    method: u16,
    crc: u32,
    compressed: usize,
    size: usize,
    offset: usize,
}

/// A parsed archive over a borrowed buffer.
pub struct ZipReader<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

impl<'a> ZipReader<'a> {
    /// Parses the central directory of `data`.
    pub fn new(data: &'a [u8]) -> Result<ZipReader<'a>, String> {
        let bad = || "not a valid zip archive".to_string();
        // The end record is within the last 22 + 65535 bytes.
        let from = data.len().saturating_sub(22 + 65535);
        let end = (from..=data.len().saturating_sub(22))
            .rev()
            .find(|&at| u32_at(data, at) == Some(END))
            .ok_or_else(bad)?;
        let count = usize::from(u16_at(data, end + 10).ok_or_else(bad)?);
        let size = u32_at(data, end + 12).ok_or_else(bad)? as usize;
        let start = u32_at(data, end + 16).ok_or_else(bad)? as usize;
        if count > MAX_ENTRIES || count == 0xFFFF || size == 0xFFFF_FFFF || start == 0xFFFF_FFFF {
            return Err("zip64 or oversized archives are not supported".to_string());
        }
        if start.checked_add(size).is_none_or(|e| e > end) {
            return Err(bad());
        }
        let mut entries = Vec::with_capacity(count);
        let mut at = start;
        for _ in 0..count {
            if u32_at(data, at) != Some(CENTRAL) {
                return Err(bad());
            }
            let flags = u16_at(data, at + 8).ok_or_else(bad)?;
            let method = u16_at(data, at + 10).ok_or_else(bad)?;
            let crc = u32_at(data, at + 16).ok_or_else(bad)?;
            let compressed = u32_at(data, at + 20).ok_or_else(bad)?;
            let size = u32_at(data, at + 24).ok_or_else(bad)?;
            let name_len = usize::from(u16_at(data, at + 28).ok_or_else(bad)?);
            let extra = usize::from(u16_at(data, at + 30).ok_or_else(bad)?);
            let comment = usize::from(u16_at(data, at + 32).ok_or_else(bad)?);
            let offset = u32_at(data, at + 42).ok_or_else(bad)?;
            let name_at = at + 46;
            let name = data.get(name_at..name_at + name_len).ok_or_else(bad)?;
            if flags & 1 != 0 {
                return Err("encrypted archives are not supported".to_string());
            }
            if compressed == 0xFFFF_FFFF || size == 0xFFFF_FFFF || offset == 0xFFFF_FFFF {
                return Err("zip64 archives are not supported".to_string());
            }
            let name = String::from_utf8_lossy(name).into_owned();
            // Two entries of one name make the picture depend on which a
            // reader finds first: refuse rather than guess.
            if entries.iter().any(|e: &Entry| e.name == name) {
                return Err(format!("the archive has two entries named {name}"));
            }
            entries.push(Entry {
                name,
                method,
                crc,
                compressed: compressed as usize,
                size: size as usize,
                offset: offset as usize,
            });
            at = name_at + name_len + extra + comment;
        }
        Ok(ZipReader { data, entries })
    }

    /// The entry names, in directory order.
    #[cfg(test)]
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// The first entry's name (OpenRaster requires `mimetype` there).
    #[cfg(test)]
    pub fn first_name(&self) -> Option<&str> {
        self.entries.first().map(|e| e.name.as_str())
    }

    /// The declared uncompressed size of `name`, if present.
    #[cfg(test)]
    pub fn size_of(&self, name: &str) -> Option<usize> {
        self.find(name).map(|e| e.size)
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// The contents of `name`, which must not exceed `limit` bytes. `Ok(None)`
    /// when there is no such entry.
    pub fn read(&self, name: &str, limit: usize) -> Result<Option<Vec<u8>>, String> {
        let Some(entry) = self.find(name) else {
            return Ok(None);
        };
        if entry.size > limit {
            return Err(format!("{name} is too large ({} bytes)", entry.size));
        }
        let bad = || format!("{name}: corrupt archive entry");
        if u32_at(self.data, entry.offset) != Some(LOCAL) {
            return Err(bad());
        }
        let name_len = usize::from(u16_at(self.data, entry.offset + 26).ok_or_else(bad)?);
        // The local header must agree with the directory about which entry
        // this is.
        let local_name = self
            .data
            .get(entry.offset + 30..entry.offset + 30 + name_len)
            .ok_or_else(bad)?;
        if local_name != entry.name.as_bytes() {
            return Err(format!("{name}: the local header names another entry"));
        }
        let extra = usize::from(u16_at(self.data, entry.offset + 28).ok_or_else(bad)?);
        let from = entry.offset + 30 + name_len + extra;
        let raw = from
            .checked_add(entry.compressed)
            .and_then(|to| self.data.get(from..to))
            .ok_or_else(bad)?;
        let bytes = match entry.method {
            0 if raw.len() == entry.size => raw.to_vec(),
            0 => return Err(bad()),
            8 => inflate(raw, entry.size).map_err(|e| format!("{name}: {e}"))?,
            m => return Err(format!("{name}: unsupported compression method {m}")),
        };
        if bytes.len() != entry.size || crc32(&bytes) != entry.crc {
            return Err(format!("{name}: checksum mismatch"));
        }
        Ok(Some(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut zip = ZipWriter::new();
        zip.add("mimetype", b"image/openraster").unwrap();
        zip.add("data/a.bin", &[1, 2, 3, 4, 5]).unwrap();
        zip.add("empty", b"").unwrap();
        zip.finish().unwrap()
    }

    #[test]
    fn a_written_archive_reads_back() {
        let bytes = sample();
        // The first entry is `mimetype`, stored, right after the 30-byte header.
        assert_eq!(&bytes[..4], b"PK\x03\x04");
        assert_eq!(&bytes[30..38], b"mimetype");
        assert_eq!(&bytes[38..54], b"image/openraster");
        let zip = ZipReader::new(&bytes).unwrap();
        assert_eq!(zip.first_name(), Some("mimetype"));
        assert_eq!(zip.names().count(), 3);
        assert_eq!(
            zip.read("data/a.bin", 100).unwrap().unwrap(),
            [1, 2, 3, 4, 5]
        );
        assert_eq!(zip.read("empty", 0).unwrap().unwrap(), b"");
        assert!(zip.read("missing", 10).unwrap().is_none());
        assert!(zip.read("data/a.bin", 4).is_err(), "limit");
        assert_eq!(zip.size_of("data/a.bin"), Some(5));
    }

    #[test]
    fn the_end_record_names_the_directory_exactly() {
        // A layout fact every foreign reader depends on.
        let bytes = sample();
        let end = bytes.len() - 22;
        assert_eq!(u32_at(&bytes, end), Some(END));
        assert_eq!(u16_at(&bytes, end + 10), Some(3));
        let start = u32_at(&bytes, end + 16).unwrap() as usize;
        let size = u32_at(&bytes, end + 12).unwrap() as usize;
        assert_eq!(start + size, end);
    }

    #[test]
    fn crc32_matches_the_known_values() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    /// `zlib.compressobj(9, DEFLATED, -15)` of `b"hello hello hello hello\n"`:
    /// one fixed-Huffman block with a back-reference.
    const FIXED: [u8; 11] = [
        0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0xb9, 0x00,
    ];

    /// The same encoder over 300 skewed bytes (`random.seed(5)`, choices from
    /// `aaaaaaabbbbccde \n`): a dynamic-Huffman block.
    const DYNAMIC: [u8; 136] = [
        0x2d, 0x8f, 0xd1, 0x0d, 0xc0, 0x40, 0x08, 0x42, 0xff, 0x9d, 0xc2, 0xd5, 0x00, 0xdd, 0x7f,
        0x85, 0x82, 0xbd, 0x36, 0x4d, 0x14, 0xf1, 0x49, 0xa9, 0xc2, 0x12, 0x80, 0x9a, 0x82, 0x8b,
        0x21, 0xe4, 0x76, 0xf1, 0x1e, 0x66, 0x08, 0x59, 0x1d, 0x37, 0x69, 0xfd, 0x8a, 0x4d, 0x74,
        0x47, 0x90, 0x30, 0xfe, 0xce, 0xcf, 0x55, 0xa9, 0xb8, 0xb1, 0xc7, 0x5d, 0x3a, 0xff, 0x21,
        0xd8, 0xc1, 0x64, 0x75, 0x3d, 0x42, 0x47, 0x73, 0x31, 0xba, 0x93, 0x8b, 0xda, 0x36, 0x94,
        0xad, 0xb8, 0x32, 0x6d, 0x96, 0xf7, 0xf5, 0x1f, 0xe3, 0x09, 0xe4, 0x9a, 0x29, 0xca, 0x5b,
        0x2b, 0x16, 0xea, 0x72, 0x9b, 0x53, 0x73, 0x47, 0x16, 0x7c, 0x68, 0x17, 0x3e, 0x38, 0xeb,
        0x68, 0x26, 0x38, 0x25, 0x27, 0x2c, 0x67, 0xa2, 0xdb, 0x19, 0x6d, 0x56, 0x2f, 0xd2, 0xc9,
        0x9a, 0xf1, 0x99, 0x56, 0x12, 0xb7, 0xbd, 0xbc, 0x3f, 0x34, 0x31, 0xdc, 0x04, 0x62, 0xe3,
        0x03,
    ];

    #[test]
    fn deflate_blocks_decode_and_the_limit_holds() {
        assert_eq!(
            inflate(&FIXED, 1000).unwrap(),
            b"hello hello hello hello\n".to_vec()
        );
        let out = inflate(&DYNAMIC, 1000).unwrap();
        assert_eq!(out.len(), 300);
        assert_eq!(crc32(&out), 0x5c42_b816);
        assert!(inflate(&DYNAMIC, 299).is_err(), "limit");
        let mut stored = vec![0x01, 5, 0, 0xFA, 0xFF];
        stored.extend_from_slice(b"abcde");
        assert_eq!(inflate(&stored, 5).unwrap(), b"abcde");
        assert!(inflate(&stored, 4).is_err(), "limit");
        stored[3] = 0;
        assert!(inflate(&stored, 99).is_err(), "bad complement");
    }

    #[test]
    fn hostile_deflate_streams_are_refused_not_panicked() {
        assert!(inflate(&[], 10).is_err());
        assert!(inflate(&[0x07], 10).is_err(), "block type 3");
        assert!(inflate(&[0xFF; 64], 1 << 20).is_err());
        assert!(inflate(&[0x03, 0x02], 10).is_err(), "back-reference");
        for n in 0..FIXED.len() {
            assert!(inflate(&FIXED[..n], 100).is_err(), "prefix {n}");
        }
        for n in 0..DYNAMIC.len() {
            let _ = inflate(&DYNAMIC[..n], 1000);
        }
        for i in 0..DYNAMIC.len() * 8 {
            let mut data = DYNAMIC;
            data[i / 8] ^= 1 << (i % 8);
            let _ = inflate(&data, 1000);
        }
    }

    /// A one-entry archive (entry `x`) whose `deflated` data inflates to `text`.
    fn deflated_archive(deflated: &[u8], text: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        push32(&mut out, LOCAL);
        for v in [20u16, 0, 8, 0, DOS_DATE] {
            push16(&mut out, v);
        }
        push32(&mut out, crc32(text));
        push32(&mut out, deflated.len() as u32);
        push32(&mut out, text.len() as u32);
        push16(&mut out, 1);
        push16(&mut out, 0);
        out.push(b'x');
        out.extend_from_slice(deflated);
        let start = out.len();
        push32(&mut out, CENTRAL);
        for v in [20u16, 20, 0, 8, 0, DOS_DATE] {
            push16(&mut out, v);
        }
        push32(&mut out, crc32(text));
        push32(&mut out, deflated.len() as u32);
        push32(&mut out, text.len() as u32);
        for v in [1u16, 0, 0, 0, 0] {
            push16(&mut out, v);
        }
        push32(&mut out, 0);
        push32(&mut out, 0);
        out.push(b'x');
        let size = out.len() - start;
        push32(&mut out, END);
        for v in [0u16, 0, 1, 1] {
            push16(&mut out, v);
        }
        push32(&mut out, size as u32);
        push32(&mut out, start as u32);
        push16(&mut out, 0);
        out
    }

    #[test]
    fn a_deflated_entry_is_read() {
        // The deflated "hello hello hello hello\n" another encoder made.
        let text = b"hello hello hello hello\n";
        let out = deflated_archive(&FIXED, text);
        let zip = ZipReader::new(&out).unwrap();
        assert_eq!(zip.read("x", 1000).unwrap().unwrap(), text);
        assert!(zip.read("x", 5).is_err());
    }

    #[test]
    fn an_entry_deflated_by_flate2_round_trips_and_a_bomb_is_cut_off() {
        use flate2::{Compression, write::DeflateEncoder};
        use std::io::Write;
        let text: Vec<u8> = (0..200_000u32).map(|i| (i % 7) as u8 + b'a').collect();
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&text).unwrap();
        let deflated = encoder.finish().unwrap();
        assert!(deflated.len() < text.len() / 10);
        let out = deflated_archive(&deflated, &text);
        let zip = ZipReader::new(&out).unwrap();
        assert_eq!(zip.read("x", 1 << 20).unwrap().unwrap(), text);
        assert!(zip.read("x", 1000).is_err(), "over the caller's limit");
        // A directory that understates the size cannot make the decoder buffer more.
        assert!(inflate(&deflated, 100).is_err());
    }

    #[test]
    fn corrupt_archives_are_errors_not_panics() {
        let good = sample();
        assert!(ZipReader::new(b"").is_err());
        assert!(ZipReader::new(b"PK\x05\x06").is_err());
        // Every truncation parses to an error or to a reader that errors on read.
        for n in 0..good.len() {
            if let Ok(zip) = ZipReader::new(&good[..n]) {
                let _ = zip.read("data/a.bin", 100);
            }
        }
        // A flipped byte in the data fails the checksum.
        let mut bad = good.clone();
        let at = bad.windows(5).position(|w| w == [1, 2, 3, 4, 5]).unwrap();
        bad[at] ^= 0xFF;
        let zip = ZipReader::new(&bad).unwrap();
        assert!(zip.read("data/a.bin", 100).is_err());
        // Flipping any single bit never panics.
        for i in 0..good.len() * 8 {
            let mut data = good.clone();
            data[i / 8] ^= 1 << (i % 8);
            if let Ok(zip) = ZipReader::new(&data) {
                for name in ["mimetype", "data/a.bin", "empty"] {
                    let _ = zip.read(name, 100);
                }
            }
        }
    }
}
