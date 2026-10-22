//! The record log's byte format: a header, then records, each `length (u32 LE)`, `crc32 of the
//! payload (u32 LE)` and the payload.
//!
//! The file is the person's to edit and other programs' to damage, and a crash can cut a write
//! short, so reading never trusts it: a record whose length is out of bounds, whose bytes are not all
//! there or whose checksum is wrong ends the log (everything before it is kept, nothing after it
//! is read: the framing is lost, so what follows cannot be told from noise). A record whose
//! frame is whole but whose content is not valid is skipped alone.

use super::model::{Entry, Id, Kind, MAX_TITLE, MAX_URL};

/// The first bytes of the file: what it is and which layout.
pub const MAGIC: &[u8; 8] = b"LZBMK\x00\x00\x01";
/// The frame's fixed part: the length and the checksum.
const FRAME: usize = 8;
/// The longest payload: a put with a title of [`MAX_TITLE`] four-byte characters and an address
/// of [`MAX_URL`] bytes. A larger length is corruption, not a big record.
pub const MAX_PAYLOAD: usize = 1 + 8 + 8 + 1 + 4 + 8 + 8 + 2 + MAX_TITLE * 4 + 2 + MAX_URL;

const PUT: u8 = 1;
const DELETE: u8 = 2;

/// One change to the set of entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// An entry is added, or replaced by this version of it.
    Put(Entry),
    Delete(Id),
}

/// What reading a log found.
#[derive(Debug, Default)]
pub struct Parsed {
    pub ops: Vec<Op>,
    /// Records whose frame was whole (valid or skipped for their content): the file's record count.
    pub records: usize,
    /// How many bytes from the start are the header and whole frames. Less than the file's length
    /// when a corrupt or cut-off tail was ignored.
    pub valid_len: usize,
    /// The file is not a bookmarks log at all (a header that is not ours).
    pub foreign: bool,
}

/// CRC-32 (IEEE 802.3, the zip and PNG one).
pub fn crc32(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[n] = c;
            n += 1;
        }
        table
    };
    let mut crc = !0u32;
    for &b in bytes {
        crc = TABLE[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// The framed bytes of `op`, to append to the file.
pub fn encode(op: &Op) -> Vec<u8> {
    let mut payload = Vec::with_capacity(64);
    match op {
        Op::Put(e) => {
            payload.push(PUT);
            payload.extend_from_slice(&e.id.to_le_bytes());
            payload.extend_from_slice(&e.parent.to_le_bytes());
            payload.push(e.kind.code());
            payload.extend_from_slice(&e.position.to_le_bytes());
            payload.extend_from_slice(&e.created.to_le_bytes());
            payload.extend_from_slice(&e.modified.to_le_bytes());
            put_str(&mut payload, &e.title);
            put_str(&mut payload, &e.url);
        }
        Op::Delete(id) => {
            payload.push(DELETE);
            payload.extend_from_slice(&id.to_le_bytes());
        }
    }
    let mut out = Vec::with_capacity(FRAME + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    // The caps keep a string far below `u16::MAX`; a longer one (not made by the store) is cut.
    let bytes = super::super::sanitize::clip_bytes(text, usize::from(u16::MAX)).as_bytes();
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Reads a whole log.
pub fn parse(bytes: &[u8]) -> Parsed {
    let mut parsed = Parsed::default();
    if bytes.is_empty() {
        return parsed;
    }
    if bytes.len() < MAGIC.len() {
        // A header cut short is a torn first write; anything else is somebody else's file.
        parsed.foreign = !MAGIC.starts_with(bytes);
        return parsed;
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        parsed.foreign = true;
        return parsed;
    }
    let mut at = MAGIC.len();
    parsed.valid_len = at;
    while let Some((payload, next)) = frame(bytes, at) {
        parsed.records += 1;
        if let Some(op) = decode(payload) {
            parsed.ops.push(op);
        }
        at = next;
        parsed.valid_len = at;
    }
    parsed
}

/// The payload of the record at `at` and where the next one starts, or `None` at the end of the
/// log or at a record that is cut, too long or fails its checksum.
fn frame(bytes: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let head = bytes.get(at..at.checked_add(FRAME)?)?;
    let len = u32::from_le_bytes(head[..4].try_into().ok()?) as usize;
    let crc = u32::from_le_bytes(head[4..].try_into().ok()?);
    if len == 0 || len > MAX_PAYLOAD {
        return None;
    }
    let start = at + FRAME;
    let end = start.checked_add(len)?;
    let payload = bytes.get(start..end)?;
    (crc32(payload) == crc).then_some((payload, end))
}

/// A cursor over a payload that never reads past its end.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2)
            .and_then(|b| b.try_into().ok())
            .map(u16::from_le_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
    }
    fn string(&mut self, max_bytes: usize) -> Option<String> {
        let len = usize::from(self.u16()?);
        if len > max_bytes {
            return None;
        }
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}

/// The operation a payload holds, or `None` when it is not a valid one (the frame was whole, the
/// content is not). Whether the entry is acceptable to a store (the caps, the address) is the
/// store's to judge.
fn decode(payload: &[u8]) -> Option<Op> {
    let mut r = Reader(payload);
    let op = match r.u8()? {
        PUT => {
            let id = r.u64()?;
            let parent = r.u64()?;
            let kind = Kind::from_code(r.u8()?)?;
            let position = r.u32()?;
            let created = r.u64()?;
            let modified = r.u64()?;
            let title = r.string(MAX_TITLE * 4)?;
            let url = r.string(MAX_URL)?;
            Op::Put(Entry {
                id,
                parent,
                kind,
                title,
                url,
                position,
                created,
                modified,
            })
        }
        DELETE => Op::Delete(r.u64()?),
        _ => return None,
    };
    // Bytes left over mean a layout this reader does not know.
    r.0.is_empty().then_some(op)
}
