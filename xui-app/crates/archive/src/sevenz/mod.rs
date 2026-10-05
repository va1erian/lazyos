//! 7z, read-only: the signature header, (encoded) headers, and folders with
//! one Copy, LZMA, LZMA2 or Deflate coder. Encrypted folders (AES) are
//! listed with their entries marked encrypted; other coder chains (BCJ,
//! delta, BZip2) list fine and fail only their own entries' reads.

mod decode;
mod files;
pub mod header;

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use crate::archive::Visitor;
use crate::entry::{Entry, EntryKind};
use crate::error::{Error, Result};
use crate::progress::{Counted, Progress};

use decode::{coder_name, folder_reader, is_encrypted};
use header::{Cursor, Header, Streams, K_ENCODED_HEADER, K_HEADER};

const SIGNATURE: [u8; 6] = [b'7', b'z', 0xbc, 0xaf, 0x27, 0x1c];
/// Largest (decoded) header accepted.
const MAX_HEADER: u64 = 256 * 1024 * 1024;
/// Encoded headers nested deeper than this are refused.
const MAX_NESTING: usize = 4;

/// Where one file's data lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Location {
    folder: usize,
    /// Byte offset inside the folder's unpacked stream.
    offset: u64,
}

/// An open 7z archive.
pub struct SevenZArchive {
    pub entries: Vec<Entry>,
    streams: Streams,
    /// Absolute offset of each folder's first pack stream.
    folder_starts: Vec<u64>,
    /// Per entry: where its data is (`None` for empty streams).
    locations: Vec<Option<Location>>,
    len: u64,
}

impl SevenZArchive {
    pub fn open(file: &mut File) -> Result<SevenZArchive> {
        let len = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;
        let mut start = [0u8; 32];
        file.read_exact(&mut start)?;
        if start[..6] != SIGNATURE {
            return Err(Error::corrupt("7z: bad signature"));
        }
        let offset = u64::from_le_bytes(start[12..20].try_into().unwrap_or([0; 8]));
        let size = u64::from_le_bytes(start[20..28].try_into().unwrap_or([0; 8]));
        let crc = u32::from_le_bytes(start[28..32].try_into().unwrap_or([0; 4]));
        let at = 32u64
            .checked_add(offset)
            .filter(|at| at.saturating_add(size) <= len && size <= MAX_HEADER)
            .ok_or_else(|| Error::corrupt("7z: header out of range"))?;
        if size == 0 {
            return Ok(SevenZArchive {
                entries: Vec::new(),
                streams: Streams::default(),
                folder_starts: Vec::new(),
                locations: Vec::new(),
                len,
            });
        }
        file.seek(SeekFrom::Start(at))?;
        let mut bytes = Vec::new();
        (&mut *file).take(size).read_to_end(&mut bytes)?;
        if crc32fast::hash(&bytes) != crc {
            return Err(Error::corrupt("7z: header CRC mismatch"));
        }
        let header = parse(file, len, bytes)?;
        build(header, len)
    }

    /// Stream the wanted entries: empty ones first, then each folder that
    /// holds a wanted file, decoded once from its start.
    pub fn visit(
        &self,
        path: &Path,
        entries: &[Entry],
        wanted: &dyn Fn(&Entry) -> bool,
        progress: &Arc<Progress>,
        visit: &mut Visitor<'_>,
    ) -> Result<()> {
        for (entry, location) in entries.iter().zip(&self.locations) {
            if location.is_none() && wanted(entry) {
                progress.begin(&entry.path);
                visit(entry, &mut io::empty())?;
            }
        }
        for folder in 0..self.streams.folders.len() {
            let members: Vec<(&Entry, Location)> = entries
                .iter()
                .zip(&self.locations)
                .filter_map(|(entry, location)| {
                    location.filter(|l| l.folder == folder).map(|l| (entry, l))
                })
                .collect();
            if !members.iter().any(|(entry, _)| wanted(entry)) {
                continue;
            }
            self.visit_folder(path, folder, &members, wanted, progress, visit)?;
        }
        Ok(())
    }

    fn visit_folder(
        &self,
        path: &Path,
        folder: usize,
        members: &[(&Entry, Location)],
        wanted: &dyn Fn(&Entry) -> bool,
        progress: &Arc<Progress>,
        visit: &mut Visitor<'_>,
    ) -> Result<()> {
        let spec = &self.streams.folders[folder];
        let mut file = File::open(path)?;
        let start = self.folder_starts[folder];
        let packed = self
            .streams
            .pack_sizes
            .get(folder_pack_index(&self.streams, folder))
            .copied()
            .unwrap_or(0);
        if start.saturating_add(packed) > self.len {
            return Err(Error::corrupt("7z: packed data past the end of the file"));
        }
        file.seek(SeekFrom::Start(start))?;
        let mut stream = match folder_reader(spec, Box::new(file.take(packed))) {
            Ok(stream) => stream,
            Err(error @ (Error::Unsupported(_) | Error::Corrupt(_))) => {
                // Every wanted member of an unreadable folder fails alone.
                let message = error.to_string();
                for (entry, _) in members.iter().filter(|(entry, _)| wanted(entry)) {
                    progress.begin(&entry.path);
                    let failure = io::Error::new(io::ErrorKind::Unsupported, message.clone());
                    visit(entry, &mut crate::archive::Failing(Some(failure)))?;
                }
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let mut position = 0u64;
        let last_wanted = members
            .iter()
            .rposition(|(entry, _)| wanted(entry))
            .unwrap_or(0);
        for (n, (entry, location)) in members.iter().enumerate() {
            if location.offset > position {
                io::copy(
                    &mut (&mut stream).take(location.offset - position),
                    &mut io::sink(),
                )?;
                position = location.offset;
            }
            if !wanted(entry) {
                continue;
            }
            progress.begin(&entry.path);
            let mut data = Checked {
                inner: Counted::new((&mut stream).take(entry.size), progress),
                hasher: crc32fast::Hasher::new(),
                crc: entry.crc,
                remaining: entry.size,
            };
            visit(entry, &mut data)?;
            // Whatever the visitor left unread still has to be decoded past.
            io::copy(&mut data, &mut io::sink())?;
            position += entry.size;
            if n == last_wanted {
                break;
            }
        }
        Ok(())
    }
}

/// The index of `folder`'s first pack stream.
fn folder_pack_index(streams: &Streams, folder: usize) -> usize {
    streams.folders[..folder]
        .iter()
        .map(|f| f.packed_streams)
        .sum()
}

/// Decode an encoded header (possibly several levels) and parse the plain one.
fn parse(file: &mut File, len: u64, mut bytes: Vec<u8>) -> Result<Header> {
    for _ in 0..MAX_NESTING {
        let mut cursor = Cursor::new(&bytes);
        match cursor.number()? {
            K_HEADER => return header::header(&mut cursor),
            K_ENCODED_HEADER => {
                let streams = header::streams_info(&mut cursor)?;
                let folder = streams
                    .folders
                    .first()
                    .ok_or_else(|| Error::corrupt("7z: empty encoded header"))?;
                if folder.unpack_size() > MAX_HEADER {
                    return Err(Error::corrupt("7z: oversized encoded header"));
                }
                let start = 32u64.saturating_add(streams.pack_pos);
                let packed = streams.pack_sizes.first().copied().unwrap_or(0);
                if start.saturating_add(packed) > len {
                    return Err(Error::corrupt("7z: encoded header out of range"));
                }
                let mut handle = file.try_clone()?;
                handle.seek(SeekFrom::Start(start))?;
                let mut decoded = Vec::new();
                folder_reader(folder, Box::new(handle.take(packed)))?
                    .take(folder.unpack_size())
                    .read_to_end(&mut decoded)?;
                if decoded.len() as u64 != folder.unpack_size() {
                    return Err(Error::corrupt("7z: short encoded header"));
                }
                if folder
                    .crc
                    .is_some_and(|crc| crc != crc32fast::hash(&decoded))
                {
                    return Err(Error::corrupt("7z: encoded header CRC mismatch"));
                }
                bytes = decoded;
            }
            _ => return Err(Error::corrupt("7z: unknown header type")),
        }
    }
    Err(Error::corrupt("7z: headers nested too deep"))
}

/// Entries and data locations from a parsed header.
fn build(header: Header, len: u64) -> Result<SevenZArchive> {
    let streams = header.streams;
    let mut folder_starts = Vec::new();
    let mut pack_index = 0usize;
    for folder in &streams.folders {
        let offset: u64 = streams.pack_sizes.iter().take(pack_index).sum();
        folder_starts.push(
            32u64
                .saturating_add(streams.pack_pos)
                .saturating_add(offset),
        );
        pack_index += folder.packed_streams;
    }
    if pack_index > streams.pack_sizes.len() {
        return Err(Error::corrupt(
            "7z: folders use more pack streams than exist",
        ));
    }
    // Hand out substreams to the files with data, in order.
    let mut slots = Vec::new();
    let mut sub = 0usize;
    for (folder, &count) in streams.substreams.iter().enumerate() {
        let mut offset = 0u64;
        for _ in 0..count {
            let size = *streams
                .sub_sizes
                .get(sub)
                .ok_or_else(|| Error::corrupt("7z: substream sizes missing"))?;
            slots.push((
                Location { folder, offset },
                size,
                streams.sub_crcs.get(sub).copied().flatten(),
            ));
            offset = offset.saturating_add(size);
            sub += 1;
        }
    }
    let mut slots = slots.into_iter();
    let mut entries = Vec::with_capacity(header.files.len());
    let mut locations = Vec::with_capacity(header.files.len());
    for (index, file) in header.files.iter().enumerate() {
        let kind = if file.is_dir && !file.has_stream {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let mut entry = Entry::new(index, &file.name, kind);
        entry.modified = file.mtime.and_then(crate::time::from_filetime);
        // The high half holds Unix mode bits when bit 15 says so.
        entry.mode = file
            .attributes
            .filter(|a| a & 0x8000 != 0)
            .map(|a| (a >> 16) & 0o7777);
        if file.has_stream {
            let (location, size, crc) = slots
                .next()
                .ok_or_else(|| Error::corrupt("7z: more files than streams"))?;
            let folder = &streams.folders[location.folder];
            entry.size = size;
            entry.crc = crc;
            entry.method = coder_name(folder);
            entry.encrypted = is_encrypted(folder);
            locations.push(Some(location));
        } else {
            locations.push(None);
        }
        entries.push(entry);
    }
    Ok(SevenZArchive {
        entries,
        streams,
        folder_starts,
        locations,
        len,
    })
}

/// A substream checked against its CRC when it has one.
struct Checked<R> {
    inner: R,
    hasher: crc32fast::Hasher,
    crc: Option<u32>,
    remaining: u64,
}

impl<R: Read> Read for Checked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.remaining = self.remaining.saturating_sub(n as u64);
        if n == 0 && !buf.is_empty() {
            if self.remaining != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "7z: stream ended early",
                ));
            }
            if let Some(crc) = self.crc.take() {
                if self.hasher.clone().finalize() != crc {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "7z: CRC mismatch",
                    ));
                }
            }
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests;
