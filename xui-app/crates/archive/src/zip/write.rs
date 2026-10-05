//! The zip writer: streamed members (local header, data, then the sizes
//! patched in place), zip64 when a size or offset needs it, Unix modes and
//! extended timestamps, and raw copies of members from another archive.

use std::io::{self, Read, Seek, SeekFrom, Write};

use flate2::write::DeflateEncoder;
use flate2::Compression;

use crate::error::{Error, Result};
use crate::format::Level;
use crate::writer::{EntryWriter, Meta};

use super::{
    Member, CENTRAL_SIG, EOCD_SIG, FLAG_DESCRIPTOR, FLAG_UTF8, LOCAL_SIG, S_IFDIR, S_IFLNK,
    ZIP64_EOCD_SIG, ZIP64_LOCATOR_SIG,
};

/// Version made by: Unix, spec 3.0.
const MADE_BY: u16 = (3 << 8) | 30;
const NEEDED: u16 = 20;
const NEEDED_ZIP64: u16 = 45;
const SATURATED: u32 = 0xffff_ffff;
/// Members above this size get zip64 fields up front (deflate can expand
/// incompressible data slightly, so the margin covers the compressed size).
const ZIP64_THRESHOLD: u64 = 0xf000_0000;
const S_IFREG: u32 = 0o100_000;

/// One member's central-directory record.
struct Central {
    flags: u16,
    method: u16,
    time: u16,
    date: u16,
    crc: u32,
    compressed: u64,
    uncompressed: u64,
    offset: u64,
    name: Vec<u8>,
    external_attrs: u32,
    mtime: Option<i64>,
}

/// Writes a zip into a seekable stream.
pub struct ZipWriter<W: Write + Seek> {
    out: W,
    pos: u64,
    level: Level,
    central: Vec<Central>,
}

impl<W: Write + Seek> ZipWriter<W> {
    pub fn new(out: W, level: Level) -> ZipWriter<W> {
        ZipWriter {
            out,
            pos: 0,
            level,
            central: Vec::new(),
        }
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }

    /// Write a local header; returns where its size fields start (offset 14)
    /// and where its zip64 extra's values start, if it has one.
    fn local_header(&mut self, record: &Central, zip64: bool) -> io::Result<(u64, Option<u64>)> {
        let start = self.pos;
        let mut extra = Vec::new();
        let mut zip64_at = None;
        if zip64 {
            zip64_at = Some(start + 30 + record.name.len() as u64 + 4);
            extra.extend_from_slice(&1u16.to_le_bytes());
            extra.extend_from_slice(&16u16.to_le_bytes());
            extra.extend_from_slice(&record.uncompressed.to_le_bytes());
            extra.extend_from_slice(&record.compressed.to_le_bytes());
        }
        time_extra(&mut extra, record.mtime);
        let (compressed, uncompressed) = if zip64 {
            (SATURATED, SATURATED)
        } else {
            (record.compressed as u32, record.uncompressed as u32)
        };
        let mut h = Vec::with_capacity(30 + record.name.len() + extra.len());
        h.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        h.extend_from_slice(&(if zip64 { NEEDED_ZIP64 } else { NEEDED }).to_le_bytes());
        h.extend_from_slice(&record.flags.to_le_bytes());
        h.extend_from_slice(&record.method.to_le_bytes());
        h.extend_from_slice(&record.time.to_le_bytes());
        h.extend_from_slice(&record.date.to_le_bytes());
        h.extend_from_slice(&record.crc.to_le_bytes());
        h.extend_from_slice(&compressed.to_le_bytes());
        h.extend_from_slice(&uncompressed.to_le_bytes());
        h.extend_from_slice(&(record.name.len() as u16).to_le_bytes());
        h.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        h.extend_from_slice(&record.name);
        h.extend_from_slice(&extra);
        self.write(&h)?;
        Ok((start + 14, zip64_at))
    }

    /// Patch a written local header's CRC and sizes.
    fn patch(&mut self, record: &Central, sizes_at: u64, zip64_at: Option<u64>) -> io::Result<()> {
        self.out.seek(SeekFrom::Start(sizes_at))?;
        self.out.write_all(&record.crc.to_le_bytes())?;
        match zip64_at {
            Some(at) => {
                self.out.seek(SeekFrom::Start(at))?;
                self.out.write_all(&record.uncompressed.to_le_bytes())?;
                self.out.write_all(&record.compressed.to_le_bytes())?;
            }
            None => {
                self.out
                    .write_all(&(record.compressed as u32).to_le_bytes())?;
                self.out
                    .write_all(&(record.uncompressed as u32).to_le_bytes())?;
            }
        }
        self.out.seek(SeekFrom::Start(self.pos))?;
        Ok(())
    }

    /// A record for a new member at the current offset.
    fn record(&self, path: &str, meta: Meta, method: u16, external_attrs: u32) -> Central {
        let (date, time) = crate::time::to_dos(meta.modified.unwrap_or(0));
        Central {
            flags: if path.is_ascii() { 0 } else { FLAG_UTF8 },
            method,
            time,
            date,
            crc: 0,
            compressed: 0,
            uncompressed: 0,
            offset: self.pos,
            name: path.as_bytes().to_vec(),
            external_attrs,
            mtime: meta.modified,
        }
    }

    /// Store `data` (exactly `size` bytes) as a member described by `record`.
    fn member(&mut self, mut record: Central, size: u64, data: &mut dyn Read) -> Result<()> {
        let zip64 = size > ZIP64_THRESHOLD || self.pos >= u64::from(SATURATED);
        record.uncompressed = size;
        let (sizes_at, zip64_at) = self.local_header(&record, zip64)?;
        let mut hashed = Hashed {
            inner: data,
            hasher: crc32fast::Hasher::new(),
            count: 0,
        };
        let written = {
            let mut counted = Counting {
                inner: &mut self.out,
                count: 0,
            };
            if record.method == 8 {
                let mut encoder =
                    DeflateEncoder::new(&mut counted, Compression::new(self.level.deflate()));
                io::copy(&mut (&mut hashed).take(size), &mut encoder)?;
                encoder.finish()?;
            } else {
                io::copy(&mut (&mut hashed).take(size), &mut counted)?;
            }
            counted.count
        };
        self.pos += written;
        if hashed.count != size {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "a file shrank while it was being archived",
            )));
        }
        if !zip64 && written >= u64::from(SATURATED) {
            return Err(Error::unsupported(
                "a member that grew past 4 GiB when compressed",
            ));
        }
        record.crc = hashed.hasher.finalize();
        record.compressed = written;
        self.patch(&record, sizes_at, zip64_at)?;
        self.central.push(record);
        Ok(())
    }

    /// Copy a member of another zip without recompressing it: `raw` yields
    /// exactly `member.compressed` bytes of its stored data.
    pub fn raw_copy(
        &mut self,
        member: &Member,
        mtime: Option<i64>,
        raw: &mut dyn Read,
    ) -> Result<()> {
        let descriptor = member.flags & FLAG_DESCRIPTOR != 0 && member.flags & 1 != 0;
        let record = Central {
            // A known size needs no descriptor; an encrypted member keeps it,
            // because its password check byte depends on the flag.
            flags: if descriptor {
                member.flags
            } else {
                member.flags & !FLAG_DESCRIPTOR
            },
            method: member.method,
            time: member.dos_time,
            date: member.dos_date,
            crc: member.crc,
            compressed: member.compressed,
            uncompressed: member.uncompressed,
            offset: self.pos,
            name: member.raw_name.clone(),
            external_attrs: member.external_attrs,
            mtime,
        };
        let zip64 = record.compressed > ZIP64_THRESHOLD
            || record.uncompressed > ZIP64_THRESHOLD
            || self.pos >= u64::from(SATURATED);
        self.local_header(&record, zip64)?;
        let copied = {
            let mut counted = Counting {
                inner: &mut self.out,
                count: 0,
            };
            io::copy(&mut raw.take(member.compressed), &mut counted)?;
            counted.count
        };
        self.pos += copied;
        if copied != member.compressed {
            return Err(Error::corrupt("zip: member data shorter than recorded"));
        }
        if descriptor {
            let mut d = Vec::new();
            d.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
            d.extend_from_slice(&record.crc.to_le_bytes());
            if zip64 {
                d.extend_from_slice(&record.compressed.to_le_bytes());
                d.extend_from_slice(&record.uncompressed.to_le_bytes());
            } else {
                d.extend_from_slice(&(record.compressed as u32).to_le_bytes());
                d.extend_from_slice(&(record.uncompressed as u32).to_le_bytes());
            }
            self.write(&d)?;
        }
        self.central.push(record);
        Ok(())
    }

    /// Write the central directory and end records; returns the stream.
    pub fn finish_inner(mut self) -> Result<W> {
        let cd_start = self.pos;
        let records = std::mem::take(&mut self.central);
        for record in &records {
            self.write(&central_header(record))?;
        }
        let cd_size = self.pos - cd_start;
        let count = records.len() as u64;
        let zip64 =
            count >= 0xffff || cd_start >= u64::from(SATURATED) || cd_size >= u64::from(SATURATED);
        if zip64 {
            let record_at = self.pos;
            let mut r = Vec::new();
            r.extend_from_slice(&ZIP64_EOCD_SIG.to_le_bytes());
            r.extend_from_slice(&44u64.to_le_bytes());
            r.extend_from_slice(&MADE_BY.to_le_bytes());
            r.extend_from_slice(&NEEDED_ZIP64.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&count.to_le_bytes());
            r.extend_from_slice(&count.to_le_bytes());
            r.extend_from_slice(&cd_size.to_le_bytes());
            r.extend_from_slice(&cd_start.to_le_bytes());
            r.extend_from_slice(&ZIP64_LOCATOR_SIG.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&record_at.to_le_bytes());
            r.extend_from_slice(&1u32.to_le_bytes());
            self.write(&r)?;
        }
        let mut e = Vec::new();
        e.extend_from_slice(&EOCD_SIG.to_le_bytes());
        e.extend_from_slice(&[0u8; 4]);
        let short_count = count.min(0xffff) as u16;
        e.extend_from_slice(&short_count.to_le_bytes());
        e.extend_from_slice(&short_count.to_le_bytes());
        e.extend_from_slice(&(cd_size.min(u64::from(SATURATED)) as u32).to_le_bytes());
        e.extend_from_slice(&(cd_start.min(u64::from(SATURATED)) as u32).to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        self.write(&e)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

impl<W: Write + Seek> EntryWriter for ZipWriter<W> {
    fn dir(&mut self, path: &str, meta: Meta) -> Result<()> {
        let mode = meta.mode.unwrap_or(0o755) & 0o7777;
        let record = self.record(
            &format!("{path}/"),
            meta,
            0,
            ((S_IFDIR | mode) << 16) | 0x10,
        );
        self.member(record, 0, &mut io::empty())
    }

    fn file(&mut self, path: &str, meta: Meta, size: u64, data: &mut dyn Read) -> Result<()> {
        let mode = meta.mode.unwrap_or(0o644) & 0o7777;
        let method = if self.level == Level::Store { 0 } else { 8 };
        let record = self.record(path, meta, method, (S_IFREG | mode) << 16);
        self.member(record, size, data)?;
        // A file that grew after its size was taken is refused, not cut.
        let mut probe = [0u8; 1];
        if data.read(&mut probe)? != 0 {
            return Err(Error::Io(io::Error::other(
                "a file grew while it was being archived",
            )));
        }
        Ok(())
    }

    fn symlink(&mut self, path: &str, meta: Meta, target: &str) -> Result<()> {
        let record = self.record(path, meta, 0, (S_IFLNK | 0o777) << 16);
        self.member(record, target.len() as u64, &mut target.as_bytes())
    }

    fn finish(self: Box<Self>) -> Result<()> {
        self.finish_inner().map(|_| ())
    }
}

/// The extended-timestamp extra field (modification time only).
fn time_extra(extra: &mut Vec<u8>, mtime: Option<i64>) {
    if let Some(mtime) = mtime.and_then(|t| i32::try_from(t).ok()) {
        extra.extend_from_slice(&0x5455u16.to_le_bytes());
        extra.extend_from_slice(&5u16.to_le_bytes());
        extra.push(1);
        extra.extend_from_slice(&mtime.to_le_bytes());
    }
}

fn central_header(record: &Central) -> Vec<u8> {
    let mut zip64 = Vec::new();
    let mut field = |value: u64| -> u32 {
        if value >= u64::from(SATURATED) {
            zip64.extend_from_slice(&value.to_le_bytes());
            SATURATED
        } else {
            value as u32
        }
    };
    let uncompressed = field(record.uncompressed);
    let compressed = field(record.compressed);
    let offset = field(record.offset);
    let mut extra = Vec::new();
    if !zip64.is_empty() {
        extra.extend_from_slice(&1u16.to_le_bytes());
        extra.extend_from_slice(&(zip64.len() as u16).to_le_bytes());
        extra.extend_from_slice(&zip64);
    }
    time_extra(&mut extra, record.mtime);
    let needed = if zip64.is_empty() {
        NEEDED
    } else {
        NEEDED_ZIP64
    };
    let mut h = Vec::with_capacity(46 + record.name.len() + extra.len());
    h.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
    h.extend_from_slice(&MADE_BY.to_le_bytes());
    h.extend_from_slice(&needed.to_le_bytes());
    h.extend_from_slice(&record.flags.to_le_bytes());
    h.extend_from_slice(&record.method.to_le_bytes());
    h.extend_from_slice(&record.time.to_le_bytes());
    h.extend_from_slice(&record.date.to_le_bytes());
    h.extend_from_slice(&record.crc.to_le_bytes());
    h.extend_from_slice(&compressed.to_le_bytes());
    h.extend_from_slice(&uncompressed.to_le_bytes());
    h.extend_from_slice(&(record.name.len() as u16).to_le_bytes());
    h.extend_from_slice(&(extra.len() as u16).to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes()); // comment
    h.extend_from_slice(&0u16.to_le_bytes()); // disk
    h.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
    h.extend_from_slice(&record.external_attrs.to_le_bytes());
    h.extend_from_slice(&offset.to_le_bytes());
    h.extend_from_slice(&record.name);
    h.extend_from_slice(&extra);
    h
}

/// Hashes and counts what is read through it.
struct Hashed<'a> {
    inner: &'a mut dyn Read,
    hasher: crc32fast::Hasher,
    count: u64,
}

impl Read for Hashed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.count += n as u64;
        Ok(n)
    }
}

/// Counts what is written through it.
struct Counting<'a, W: Write> {
    inner: &'a mut W,
    count: u64,
}

impl<W: Write> Write for Counting<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
