//! The 7z header grammar: streams info (pack, folders, substreams) and files
//! info, parsed from a byte buffer with every count checked against the bytes
//! that are left, so a hostile header cannot make the parser allocate more
//! than its own size.

use crate::error::{Error, Result};

pub const K_END: u64 = 0x00;
pub const K_HEADER: u64 = 0x01;
const K_ARCHIVE_PROPERTIES: u64 = 0x02;
const K_ADDITIONAL_STREAMS: u64 = 0x03;
const K_MAIN_STREAMS: u64 = 0x04;
const K_FILES_INFO: u64 = 0x05;
const K_PACK_INFO: u64 = 0x06;
const K_UNPACK_INFO: u64 = 0x07;
const K_SUBSTREAMS_INFO: u64 = 0x08;
const K_SIZE: u64 = 0x09;
const K_CRC: u64 = 0x0a;
const K_FOLDER: u64 = 0x0b;
const K_CODERS_UNPACK_SIZE: u64 = 0x0c;
const K_NUM_UNPACK_STREAM: u64 = 0x0d;
pub const K_ENCODED_HEADER: u64 = 0x17;

/// Coders per folder this parser accepts (7-Zip itself writes at most 4).
const MAX_CODERS: usize = 8;

/// A cursor over header bytes.
pub struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

pub(super) fn short() -> Error {
    Error::corrupt("7z: truncated header")
}

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Cursor<'a> {
        Cursor { bytes, at: 0 }
    }

    pub(super) fn left(&self) -> usize {
        self.bytes.len() - self.at
    }

    pub fn byte(&mut self) -> Result<u8> {
        let byte = *self.bytes.get(self.at).ok_or_else(short)?;
        self.at += 1;
        Ok(byte)
    }

    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.left() {
            return Err(short());
        }
        let slice = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(slice)
    }

    pub(super) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| short())?,
        ))
    }

    pub(super) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| short())?,
        ))
    }

    /// 7z's variable-length number: the first byte's leading one bits count
    /// the extra bytes.
    pub fn number(&mut self) -> Result<u64> {
        let first = self.byte()?;
        let mut mask = 0x80u8;
        let mut value = 0u64;
        for i in 0..8 {
            if first & mask == 0 {
                let high = u64::from(first & mask.wrapping_sub(1));
                return Ok(value | (high << (8 * i)));
            }
            value |= u64::from(self.byte()?) << (8 * i);
            mask >>= 1;
        }
        Ok(value)
    }

    /// A count that each item will consume at least `per_item_bits` of the
    /// remaining input for, so a huge count fails here instead of allocating.
    pub(super) fn count(&mut self, per_item_bits: usize) -> Result<usize> {
        let n = self.number()?;
        let n = usize::try_from(n).map_err(|_| short())?;
        if per_item_bits > 0 && n > self.left().saturating_mul(8) / per_item_bits {
            return Err(Error::corrupt("7z: a count larger than the header"));
        }
        Ok(n)
    }

    pub(super) fn bits(&mut self, n: usize) -> Result<Vec<bool>> {
        let bytes = self.take(n.div_ceil(8))?;
        Ok((0..n)
            .map(|i| bytes[i / 8] & (0x80 >> (i % 8)) != 0)
            .collect())
    }

    /// An "all defined" byte, else a bit vector.
    pub(super) fn defined(&mut self, n: usize) -> Result<Vec<bool>> {
        if self.byte()? != 0 {
            Ok(vec![true; n])
        } else {
            self.bits(n)
        }
    }

    fn digests(&mut self, n: usize) -> Result<Vec<Option<u32>>> {
        let defined = self.defined(n)?;
        defined
            .into_iter()
            .map(|d| if d { self.u32().map(Some) } else { Ok(None) })
            .collect()
    }

    fn expect(&mut self, id: u64) -> Result<()> {
        if self.number()? != id {
            return Err(Error::corrupt("7z: unexpected header property"));
        }
        Ok(())
    }
}

/// One coder of a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coder {
    pub id: Vec<u8>,
    pub props: Vec<u8>,
    pub in_streams: u64,
    pub out_streams: u64,
}

/// A folder: a chain of coders whose output is one solid stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Folder {
    pub coders: Vec<Coder>,
    pub bind_pairs: usize,
    pub packed_streams: usize,
    /// The output size of every coder output, in order.
    pub unpack_sizes: Vec<u64>,
    pub crc: Option<u32>,
}

impl Folder {
    /// The folder's final output size (the unbound output: with a single
    /// coder, its only one).
    pub fn unpack_size(&self) -> u64 {
        self.unpack_sizes.last().copied().unwrap_or(0)
    }
}

/// Streams info: where packed data is and how it unpacks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Streams {
    pub pack_pos: u64,
    pub pack_sizes: Vec<u64>,
    pub folders: Vec<Folder>,
    /// Per folder: how many files' data it holds.
    pub substreams: Vec<usize>,
    /// Every substream's size and CRC, folder after folder.
    pub sub_sizes: Vec<u64>,
    pub sub_crcs: Vec<Option<u32>>,
}

/// A parsed header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub streams: Streams,
    pub files: Vec<super::files::FileRecord>,
}

pub fn streams_info(c: &mut Cursor<'_>) -> Result<Streams> {
    let mut streams = Streams::default();
    let mut has_substreams = false;
    loop {
        match c.number()? {
            K_END => break,
            K_PACK_INFO => pack_info(c, &mut streams)?,
            K_UNPACK_INFO => unpack_info(c, &mut streams)?,
            K_SUBSTREAMS_INFO => {
                substreams_info(c, &mut streams)?;
                has_substreams = true;
            }
            _ => return Err(Error::corrupt("7z: unknown streams property")),
        }
    }
    if !has_substreams {
        // One stream per folder, carrying the folder's own CRC.
        streams.substreams = vec![1; streams.folders.len()];
        streams.sub_sizes = streams.folders.iter().map(Folder::unpack_size).collect();
        streams.sub_crcs = streams.folders.iter().map(|f| f.crc).collect();
    }
    Ok(streams)
}

fn pack_info(c: &mut Cursor<'_>, streams: &mut Streams) -> Result<()> {
    streams.pack_pos = c.number()?;
    let count = c.count(8)?;
    loop {
        match c.number()? {
            K_END => break,
            K_SIZE => {
                streams.pack_sizes = (0..count).map(|_| c.number()).collect::<Result<_>>()?;
            }
            K_CRC => {
                c.digests(count)?;
            }
            _ => return Err(Error::corrupt("7z: unknown pack property")),
        }
    }
    if streams.pack_sizes.len() != count {
        return Err(Error::corrupt("7z: pack sizes missing"));
    }
    Ok(())
}

fn folder(c: &mut Cursor<'_>) -> Result<Folder> {
    let count = c.count(8)?;
    if count == 0 || count > MAX_CODERS {
        return Err(Error::unsupported("7z: a folder with that many coders"));
    }
    let mut folder = Folder::default();
    let (mut total_in, mut total_out) = (0u64, 0u64);
    for _ in 0..count {
        let flags = c.byte()?;
        if flags & 0x80 != 0 {
            return Err(Error::unsupported("7z: alternative coder methods"));
        }
        let id = c.take(usize::from(flags & 0x0f))?.to_vec();
        let (in_streams, out_streams) = if flags & 0x10 != 0 {
            (c.number()?, c.number()?)
        } else {
            (1, 1)
        };
        if in_streams > 32 || out_streams > 32 {
            return Err(Error::unsupported("7z: a coder with that many streams"));
        }
        let props = if flags & 0x20 != 0 {
            let size = c.count(8)?;
            c.take(size)?.to_vec()
        } else {
            Vec::new()
        };
        total_in += in_streams;
        total_out += out_streams;
        folder.coders.push(Coder {
            id,
            props,
            in_streams,
            out_streams,
        });
    }
    let bind_pairs = total_out
        .checked_sub(1)
        .ok_or_else(|| Error::corrupt("7z: a folder without output"))?;
    for _ in 0..bind_pairs {
        c.number()?;
        c.number()?;
    }
    let packed = total_in
        .checked_sub(bind_pairs)
        .ok_or_else(|| Error::corrupt("7z: bad bind pairs"))?;
    if packed > 1 {
        for _ in 0..packed {
            c.number()?;
        }
    }
    folder.bind_pairs = bind_pairs as usize;
    folder.packed_streams = packed as usize;
    Ok(folder)
}

fn unpack_info(c: &mut Cursor<'_>, streams: &mut Streams) -> Result<()> {
    c.expect(K_FOLDER)?;
    let count = c.count(8)?;
    if c.byte()? != 0 {
        return Err(Error::unsupported("7z: external folder data"));
    }
    streams.folders = (0..count).map(|_| folder(c)).collect::<Result<_>>()?;
    c.expect(K_CODERS_UNPACK_SIZE)?;
    for folder in &mut streams.folders {
        let outputs: u64 = folder.coders.iter().map(|coder| coder.out_streams).sum();
        folder.unpack_sizes = (0..outputs).map(|_| c.number()).collect::<Result<_>>()?;
    }
    loop {
        match c.number()? {
            K_END => break,
            K_CRC => {
                let crcs = c.digests(count)?;
                for (folder, crc) in streams.folders.iter_mut().zip(crcs) {
                    folder.crc = crc;
                }
            }
            _ => return Err(Error::corrupt("7z: unknown unpack property")),
        }
    }
    Ok(())
}

fn substreams_info(c: &mut Cursor<'_>, streams: &mut Streams) -> Result<()> {
    streams.substreams = vec![1; streams.folders.len()];
    let mut id = c.number()?;
    if id == K_NUM_UNPACK_STREAM {
        for slot in streams.substreams.iter_mut() {
            *slot = c.count(0)?;
        }
        id = c.number()?;
    }
    let total: usize = streams
        .substreams
        .iter()
        .try_fold(0usize, |sum, &n| sum.checked_add(n))
        .ok_or_else(|| Error::corrupt("7z: too many substreams"))?;
    if total > c.left().saturating_add(streams.folders.len()) {
        return Err(Error::corrupt(
            "7z: more substreams than the header can describe",
        ));
    }
    let mut sizes = Vec::new();
    for (folder, &n) in streams.folders.iter().zip(&streams.substreams) {
        if n == 0 {
            continue;
        }
        let mut sum = 0u64;
        if id == K_SIZE {
            for _ in 1..n {
                let size = c.number()?;
                sum = sum
                    .checked_add(size)
                    .ok_or_else(|| Error::corrupt("7z: substream sizes overflow"))?;
                sizes.push(size);
            }
        } else if n > 1 {
            return Err(Error::corrupt("7z: substream sizes missing"));
        }
        let last = folder
            .unpack_size()
            .checked_sub(sum)
            .ok_or_else(|| Error::corrupt("7z: substreams larger than their folder"))?;
        sizes.push(last);
    }
    if id == K_SIZE {
        id = c.number()?;
    }
    // Folders with one substream and a folder CRC already have theirs.
    let mut crcs = Vec::new();
    let mut unknown = 0;
    for (folder, &n) in streams.folders.iter().zip(&streams.substreams) {
        if n == 1 && folder.crc.is_some() {
            crcs.push(folder.crc);
        } else {
            for _ in 0..n {
                crcs.push(None);
                unknown += 1;
            }
        }
    }
    loop {
        match id {
            K_END => break,
            K_CRC => {
                let mut digests = c.digests(unknown)?.into_iter();
                for slot in crcs.iter_mut().filter(|slot| slot.is_none()) {
                    *slot = digests.next().flatten();
                }
            }
            _ => return Err(Error::corrupt("7z: unknown substreams property")),
        }
        id = c.number()?;
    }
    streams.sub_sizes = sizes;
    streams.sub_crcs = crcs;
    Ok(())
}

/// A plain (not encoded) header, after its `K_HEADER` id.
pub fn header(c: &mut Cursor<'_>) -> Result<Header> {
    let mut header = Header::default();
    loop {
        match c.number()? {
            K_END => break,
            K_ARCHIVE_PROPERTIES => loop {
                if c.number()? == K_END {
                    break;
                }
                let size = usize::try_from(c.number()?).map_err(|_| short())?;
                c.take(size)?;
            },
            K_ADDITIONAL_STREAMS => {
                streams_info(c)?;
            }
            K_MAIN_STREAMS => header.streams = streams_info(c)?,
            K_FILES_INFO => header.files = super::files::files_info(c)?,
            _ => return Err(Error::corrupt("7z: unknown header property")),
        }
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_decode_in_every_width() {
        assert_eq!(Cursor::new(&[0x7f]).number().unwrap(), 0x7f);
        assert_eq!(Cursor::new(&[0x80, 0xff]).number().unwrap(), 0xff);
        assert_eq!(
            Cursor::new(&[0xc1, 0x02, 0x03]).number().unwrap(),
            0x01_0302
        );
        let nine = [0xff, 1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(Cursor::new(&nine).number().unwrap(), 0x0807_0605_0403_0201);
    }

    #[test]
    fn bit_vectors_are_msb_first() {
        assert_eq!(
            Cursor::new(&[0b1010_0000]).bits(3).unwrap(),
            vec![true, false, true]
        );
    }
}
