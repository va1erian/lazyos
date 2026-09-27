//! Messenger parcel codec (issue #65).
//!
//! The wire format is specified in `docs/messenger.md` section 4: a fixed
//! header, then a self-describing TLV body, then arrays of transferred handles
//! and shared-buffer descriptors. Everything is little-endian.
//!
//! Two properties matter more than cleverness:
//!
//! * **Forward compatibility.** A reader must skip TLV fields whose kind it does
//!   not know, so a newer sender can add fields without breaking older peers.
//! * **Total robustness.** `decode` is the kernel's attack surface. It must
//!   never panic or read out of bounds on malformed input, and every limit is
//!   explicit. The `fuzz` tests hammer this.
//!
//! The crate is `no_std` (plus `alloc`) so the kernel and ring-3 programs share
//! one implementation; on a host it is tested with `cargo test`.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt;

/// Wire version written into new parcels.
pub const VERSION: u16 = 1;
/// Fixed header size in bytes.
pub const HEADER_SIZE: usize = 48;
/// Largest parcel we will encode or accept.
pub const MAX_PARCEL_BYTES: usize = 1 << 20;
/// Largest TLV body.
pub const MAX_BODY_BYTES: usize = 1 << 20;
/// Largest number of transferred handles per parcel.
pub const MAX_HANDLES: usize = 64;
/// Largest number of shared-buffer descriptors per parcel.
pub const MAX_BUFFERS: usize = 64;
/// Largest number of top-level TLV fields.
pub const MAX_FIELDS: usize = 1024;
/// Largest nesting depth for composite TLV values (array/struct/map/option).
pub const MAX_DEPTH: u8 = 16;
/// Size of one shared-buffer descriptor.
const BUFFER_DESC_SIZE: usize = 28;

/// Parcel header flags (see `docs/messenger.md`).
pub mod flags {
    /// A reply is expected (synchronous transaction).
    pub const SYNC: u16 = 1 << 0;
    /// Fire-and-forget.
    pub const ONE_WAY: u16 = 1 << 1;
    /// Do not error if the callee dies before replying.
    pub const NO_REPLY_IF_DEAD: u16 = 1 << 2;
    /// Nested transactions are permitted.
    pub const ALLOW_NESTED: u16 = 1 << 3;
    /// The callee requires kernel credentials.
    pub const CRED_REQUIRED: u16 = 1 << 4;
    /// Emit trace events for this transaction.
    pub const TRACE: u16 = 1 << 5;
}

/// Why a parcel could not be encoded or decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// Input ended in the middle of a structure.
    Truncated,
    /// A size limit (parcel, body, handles, buffers, fields, depth) was exceeded.
    TooLarge,
    /// Nesting exceeded [`MAX_DEPTH`].
    TooDeep,
    /// A value's length does not match its kind (e.g. `u64` with 4 bytes).
    BadValue,
    /// A string field was not valid UTF-8.
    BadString,
    /// The header version is not supported.
    BadVersion,
    /// Trailing bytes after a well-formed parcel.
    TrailingBytes,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::Truncated => "parcel ended in the middle of a field",
            Error::TooLarge => "parcel exceeds a Messenger size limit",
            Error::TooDeep => "parcel nesting is too deep",
            Error::BadValue => "field value has the wrong size for its kind",
            Error::BadString => "string field is not valid UTF-8",
            Error::BadVersion => "unsupported parcel version",
            Error::TrailingBytes => "parcel has trailing bytes after its body",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// Fixed header of a parcel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Header {
    pub version: u16,
    pub flags: u16,
    pub interface_id: u64,
    pub method: u32,
    pub txn_id: u64,
    pub reply_to: u64,
    pub deadline_ns: u64,
}

/// A shared-buffer descriptor carried by a parcel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct BufferDesc {
    pub handle: u64,
    pub offset: u64,
    pub len: u64,
    pub flags: u32,
}

/// A complete message: header, TLV body, transferred handles, shared buffers.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Parcel {
    pub header: Header,
    pub body: Vec<u8>,
    pub handles: Vec<u64>,
    pub buffers: Vec<BufferDesc>,
}

impl Parcel {
    /// Encode into `out`, replacing its contents.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let body_len = self.body.len();
        if body_len > MAX_BODY_BYTES
            || self.handles.len() > MAX_HANDLES
            || self.buffers.len() > MAX_BUFFERS
        {
            return Err(Error::TooLarge);
        }
        let total = HEADER_SIZE
            .checked_add(body_len)
            .and_then(|n| n.checked_add(self.handles.len() * 8))
            .and_then(|n| n.checked_add(self.buffers.len() * BUFFER_DESC_SIZE))
            .ok_or(Error::TooLarge)?;
        if total > MAX_PARCEL_BYTES {
            return Err(Error::TooLarge);
        }

        out.clear();
        out.reserve(total);
        let h = &self.header;
        out.extend_from_slice(&h.version.to_le_bytes());
        out.extend_from_slice(&h.flags.to_le_bytes());
        out.extend_from_slice(&h.interface_id.to_le_bytes());
        out.extend_from_slice(&h.method.to_le_bytes());
        out.extend_from_slice(&h.txn_id.to_le_bytes());
        out.extend_from_slice(&h.reply_to.to_le_bytes());
        out.extend_from_slice(&h.deadline_ns.to_le_bytes());
        out.extend_from_slice(&(body_len as u32).to_le_bytes());
        out.extend_from_slice(&(self.handles.len() as u16).to_le_bytes());
        out.extend_from_slice(&(self.buffers.len() as u16).to_le_bytes());
        debug_assert_eq!(out.len(), HEADER_SIZE);

        out.extend_from_slice(&self.body);
        for handle in &self.handles {
            out.extend_from_slice(&handle.to_le_bytes());
        }
        for buffer in &self.buffers {
            out.extend_from_slice(&buffer.handle.to_le_bytes());
            out.extend_from_slice(&buffer.offset.to_le_bytes());
            out.extend_from_slice(&buffer.len.to_le_bytes());
            out.extend_from_slice(&buffer.flags.to_le_bytes());
        }
        Ok(())
    }

    /// Decode a parcel, validating every length and limit.
    pub fn decode(bytes: &[u8]) -> Result<Parcel, Error> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Truncated);
        }
        if bytes.len() > MAX_PARCEL_BYTES {
            return Err(Error::TooLarge);
        }
        let version = read_u16(bytes, 0)?;
        if version != VERSION {
            return Err(Error::BadVersion);
        }
        let header = Header {
            version,
            flags: read_u16(bytes, 2)?,
            interface_id: read_u64(bytes, 4)?,
            method: read_u32(bytes, 12)?,
            txn_id: read_u64(bytes, 16)?,
            reply_to: read_u64(bytes, 24)?,
            deadline_ns: read_u64(bytes, 32)?,
        };
        let body_len = read_u32(bytes, 40)? as usize;
        let handle_count = read_u16(bytes, 44)? as usize;
        let buffer_count = read_u16(bytes, 46)? as usize;
        if body_len > MAX_BODY_BYTES || handle_count > MAX_HANDLES || buffer_count > MAX_BUFFERS {
            return Err(Error::TooLarge);
        }

        let handles_at = HEADER_SIZE + body_len;
        let buffers_at = handles_at + handle_count * 8;
        let end = buffers_at + buffer_count * BUFFER_DESC_SIZE;
        if end > bytes.len() {
            return Err(Error::Truncated);
        }
        if end != bytes.len() {
            return Err(Error::TrailingBytes);
        }

        let body = bytes[HEADER_SIZE..handles_at].to_vec();
        let mut handles = Vec::with_capacity(handle_count);
        for i in 0..handle_count {
            handles.push(read_u64(bytes, handles_at + i * 8)?);
        }
        let mut buffers = Vec::with_capacity(buffer_count);
        for i in 0..buffer_count {
            let at = buffers_at + i * BUFFER_DESC_SIZE;
            buffers.push(BufferDesc {
                handle: read_u64(bytes, at)?,
                offset: read_u64(bytes, at + 8)?,
                len: read_u64(bytes, at + 16)?,
                flags: read_u32(bytes, at + 24)?,
            });
        }
        Ok(Parcel {
            header,
            body,
            handles,
            buffers,
        })
    }
}

/// The kind of a TLV field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Bool = 1,
    I32 = 2,
    I64 = 3,
    U32 = 4,
    U64 = 5,
    F64 = 6,
    String = 7,
    Bytes = 8,
    Array = 9,
    Struct = 10,
    Map = 11,
    Option = 12,
    Error = 13,
    Handle = 14,
    Buffer = 15,
}

impl Kind {
    fn from_tag(tag: u8) -> Option<Kind> {
        Some(match tag {
            1 => Kind::Bool,
            2 => Kind::I32,
            3 => Kind::I64,
            4 => Kind::U32,
            5 => Kind::U64,
            6 => Kind::F64,
            7 => Kind::String,
            8 => Kind::Bytes,
            9 => Kind::Array,
            10 => Kind::Struct,
            11 => Kind::Map,
            12 => Kind::Option,
            13 => Kind::Error,
            14 => Kind::Handle,
            15 => Kind::Buffer,
            _ => return None,
        })
    }
}

/// Pack a field tag: kind in the low 8 bits, a 16-bit field id above it.
fn tag(kind: Kind, id: u16) -> u32 {
    kind as u32 | ((id as u32) << 8)
}

/// Builds a TLV body field by field.
#[derive(Default)]
pub struct Encoder {
    buf: Vec<u8>,
    fields: usize,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// The encoded body so far.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Finish and return the body bytes.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    fn push(&mut self, kind: Kind, id: u16, payload: &[u8]) -> Result<(), Error> {
        if self.fields >= MAX_FIELDS {
            return Err(Error::TooLarge);
        }
        let len = u32::try_from(payload.len()).map_err(|_| Error::TooLarge)?;
        if self.buf.len() + 8 + payload.len() > MAX_BODY_BYTES {
            return Err(Error::TooLarge);
        }
        self.fields += 1;
        self.buf.extend_from_slice(&tag(kind, id).to_le_bytes());
        self.buf.extend_from_slice(&len.to_le_bytes());
        self.buf.extend_from_slice(payload);
        Ok(())
    }

    pub fn bool(&mut self, id: u16, value: bool) -> Result<(), Error> {
        self.push(Kind::Bool, id, &[value as u8])
    }

    pub fn i32(&mut self, id: u16, value: i32) -> Result<(), Error> {
        self.push(Kind::I32, id, &value.to_le_bytes())
    }

    pub fn i64(&mut self, id: u16, value: i64) -> Result<(), Error> {
        self.push(Kind::I64, id, &value.to_le_bytes())
    }

    pub fn u32(&mut self, id: u16, value: u32) -> Result<(), Error> {
        self.push(Kind::U32, id, &value.to_le_bytes())
    }

    pub fn u64(&mut self, id: u16, value: u64) -> Result<(), Error> {
        self.push(Kind::U64, id, &value.to_le_bytes())
    }

    pub fn f64(&mut self, id: u16, value: f64) -> Result<(), Error> {
        self.push(Kind::F64, id, &value.to_le_bytes())
    }

    pub fn string(&mut self, id: u16, value: &str) -> Result<(), Error> {
        self.push(Kind::String, id, value.as_bytes())
    }

    pub fn bytes(&mut self, id: u16, value: &[u8]) -> Result<(), Error> {
        self.push(Kind::Bytes, id, value)
    }

    /// A nested list of fields.
    pub fn array(&mut self, id: u16, nested: &Encoder) -> Result<(), Error> {
        self.push(Kind::Array, id, nested.as_bytes())
    }

    /// A nested record of fields.
    pub fn record(&mut self, id: u16, nested: &Encoder) -> Result<(), Error> {
        self.push(Kind::Struct, id, nested.as_bytes())
    }

    /// A nested key/value pair set (keys and values alternate as TLVs).
    pub fn map(&mut self, id: u16, nested: &Encoder) -> Result<(), Error> {
        self.push(Kind::Map, id, nested.as_bytes())
    }

    /// An optional nested value: `None` encodes as an empty payload.
    pub fn option(&mut self, id: u16, value: Option<&Encoder>) -> Result<(), Error> {
        self.push(Kind::Option, id, value.map(|e| e.as_bytes()).unwrap_or(&[]))
    }

    /// A structured error: a 32-bit code followed by a UTF-8 message.
    pub fn error(&mut self, id: u16, code: u32, message: &str) -> Result<(), Error> {
        let mut payload = Vec::with_capacity(4 + message.len());
        payload.extend_from_slice(&code.to_le_bytes());
        payload.extend_from_slice(message.as_bytes());
        self.push(Kind::Error, id, &payload)
    }

    pub fn handle(&mut self, id: u16, handle: u64) -> Result<(), Error> {
        self.push(Kind::Handle, id, &handle.to_le_bytes())
    }

    pub fn buffer(&mut self, id: u16, buffer: &BufferDesc) -> Result<(), Error> {
        let mut payload = [0u8; BUFFER_DESC_SIZE];
        payload[0..8].copy_from_slice(&buffer.handle.to_le_bytes());
        payload[8..16].copy_from_slice(&buffer.offset.to_le_bytes());
        payload[16..24].copy_from_slice(&buffer.len.to_le_bytes());
        payload[24..28].copy_from_slice(&buffer.flags.to_le_bytes());
        self.push(Kind::Buffer, id, &payload)
    }
}

/// One decoded TLV field borrowing from the body.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Field<'a> {
    pub kind: Kind,
    pub id: u16,
    pub payload: &'a [u8],
}

impl<'a> Field<'a> {
    pub fn as_bool(&self) -> Result<bool, Error> {
        match self.payload {
            [0] => Ok(false),
            [1] => Ok(true),
            _ => Err(Error::BadValue),
        }
    }
    pub fn as_u32(&self) -> Result<u32, Error> {
        read_u32(self.payload, 0)
    }
    pub fn as_u64(&self) -> Result<u64, Error> {
        read_u64(self.payload, 0)
    }
    pub fn as_i32(&self) -> Result<i32, Error> {
        Ok(self.as_u32()? as i32)
    }
    pub fn as_i64(&self) -> Result<i64, Error> {
        Ok(self.as_u64()? as i64)
    }
    pub fn as_f64(&self) -> Result<f64, Error> {
        Ok(f64::from_bits(self.as_u64()?))
    }
    pub fn as_str(&self) -> Result<&'a str, Error> {
        core::str::from_utf8(self.payload).map_err(|_| Error::BadString)
    }
    pub fn as_bytes(&self) -> &'a [u8] {
        self.payload
    }
    /// Decode a nested composite value (array/struct/map/option).
    pub fn nested(&self, depth: u8) -> Result<Decoder<'a>, Error> {
        if depth >= MAX_DEPTH {
            return Err(Error::TooDeep);
        }
        Ok(Decoder {
            buf: self.payload,
            pos: 0,
        })
    }
    /// Split an `Error` field into `(code, message)`.
    pub fn error_parts(&self) -> Result<(u32, &'a str), Error> {
        if self.payload.len() < 4 {
            return Err(Error::BadValue);
        }
        let code = read_u32(self.payload, 0)?;
        let message = core::str::from_utf8(&self.payload[4..]).map_err(|_| Error::BadString)?;
        Ok((code, message))
    }
    pub fn as_handle(&self) -> Result<u64, Error> {
        self.as_u64()
    }
    pub fn as_buffer(&self) -> Result<BufferDesc, Error> {
        if self.payload.len() != BUFFER_DESC_SIZE {
            return Err(Error::BadValue);
        }
        Ok(BufferDesc {
            handle: read_u64(self.payload, 0)?,
            offset: read_u64(self.payload, 8)?,
            len: read_u64(self.payload, 16)?,
            flags: read_u32(self.payload, 24)?,
        })
    }
}

/// Iterates TLV fields. Unknown kinds are skipped, so newer senders stay
/// compatible with older readers.
pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Decoder { buf, pos: 0 }
    }

    /// Next known field, or `None` at the end.
    pub fn next(&mut self) -> Result<Option<Field<'a>>, Error> {
        loop {
            if self.pos == self.buf.len() {
                return Ok(None);
            }
            if self.buf.len() - self.pos < 8 {
                return Err(Error::Truncated);
            }
            let raw = read_u32(self.buf, self.pos)?;
            let len = read_u32(self.buf, self.pos + 4)? as usize;
            let start = self.pos + 8;
            let end = start.checked_add(len).ok_or(Error::TooLarge)?;
            if end > self.buf.len() {
                return Err(Error::Truncated);
            }
            self.pos = end;
            match Kind::from_tag(raw as u8) {
                Some(kind) => {
                    return Ok(Some(Field {
                        kind,
                        id: (raw >> 8) as u16,
                        payload: &self.buf[start..end],
                    }))
                }
                None => continue, // unknown kind: skip
            }
        }
    }
}

/// Read a little-endian `u16`, or `Truncated`.
fn read_u16(buf: &[u8], at: usize) -> Result<u16, Error> {
    let bytes = buf.get(at..at + 2).ok_or(Error::Truncated)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Read a little-endian `u32`, or `Truncated`.
fn read_u32(buf: &[u8], at: usize) -> Result<u32, Error> {
    let bytes = buf.get(at..at + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Read a little-endian `u64`, or `Truncated`.
fn read_u64(buf: &[u8], at: usize) -> Result<u64, Error> {
    let bytes = buf.get(at..at + 8).ok_or(Error::Truncated)?;
    let mut array = [0u8; 8];
    array.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(array))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_parcel() -> Parcel {
        let mut body = Encoder::new();
        body.bool(1, true).unwrap();
        body.i32(2, -7).unwrap();
        body.i64(3, -9_000_000_000).unwrap();
        body.u32(4, 42).unwrap();
        body.u64(5, u64::MAX).unwrap();
        body.f64(6, 3.5).unwrap();
        body.string(7, "hello messenger").unwrap();
        body.bytes(8, &[0, 1, 2, 255]).unwrap();
        let mut inner = Encoder::new();
        inner.u32(1, 10).unwrap();
        inner.u32(2, 20).unwrap();
        body.array(9, &inner).unwrap();
        let mut record = Encoder::new();
        record.string(1, "name").unwrap();
        body.record(10, &record).unwrap();
        let mut map = Encoder::new();
        map.u32(1, 1).unwrap();
        map.u32(2, 2).unwrap();
        body.map(11, &map).unwrap();
        body.option(12, None).unwrap();
        body.option(13, Some(&record)).unwrap();
        body.error(14, 38, "not implemented").unwrap();
        body.handle(15, 0xdead_beef).unwrap();
        body.buffer(
            16,
            &BufferDesc {
                handle: 7,
                offset: 4096,
                len: 8192,
                flags: 3,
            },
        )
        .unwrap();

        Parcel {
            header: Header {
                version: VERSION,
                flags: flags::SYNC | flags::TRACE,
                interface_id: 0x1234_5678_9abc_def0,
                method: 9,
                txn_id: 0xfeed,
                reply_to: 0,
                deadline_ns: 1_000_000,
            },
            body: body.finish(),
            handles: vec![1, 2, 3],
            buffers: vec![BufferDesc {
                handle: 7,
                offset: 4096,
                len: 8192,
                flags: 3,
            }],
        }
    }

    #[test]
    fn round_trip_parcel() {
        let parcel = sample_parcel();
        let mut bytes = Vec::new();
        parcel.encode(&mut bytes).unwrap();
        let decoded = Parcel::decode(&bytes).unwrap();
        assert_eq!(decoded, parcel);
    }

    #[test]
    fn every_kind_decodes() {
        let parcel = sample_parcel();
        let mut bytes = Vec::new();
        parcel.encode(&mut bytes).unwrap();
        let decoded = Parcel::decode(&bytes).unwrap();

        let mut seen = 0usize;
        let mut decoder = Decoder::new(&decoded.body);
        while let Some(field) = decoder.next().unwrap() {
            match field.kind {
                Kind::Bool => assert!(field.as_bool().unwrap()),
                Kind::I32 => assert_eq!(field.as_i32().unwrap(), -7),
                Kind::I64 => assert_eq!(field.as_i64().unwrap(), -9_000_000_000),
                Kind::U32 => assert_eq!(field.as_u32().unwrap(), 42),
                Kind::U64 => assert_eq!(field.as_u64().unwrap(), u64::MAX),
                Kind::F64 => assert_eq!(field.as_f64().unwrap(), 3.5),
                Kind::String => assert_eq!(field.as_str().unwrap(), "hello messenger"),
                Kind::Bytes => assert_eq!(field.as_bytes(), &[0, 1, 2, 255]),
                Kind::Array => {
                    let mut nested = field.nested(0).unwrap();
                    assert_eq!(nested.count(), 2);
                }
                Kind::Struct => {
                    let mut nested = field.nested(0).unwrap();
                    assert_eq!(nested.count(), 1);
                }
                Kind::Map => {
                    let mut nested = field.nested(0).unwrap();
                    assert_eq!(nested.count(), 2);
                }
                Kind::Option => {
                    let mut nested = field.nested(0).unwrap();
                    assert_eq!(
                        nested.count() as u8,
                        if field.payload.is_empty() { 0 } else { 1 }
                    );
                }
                Kind::Error => {
                    let (code, message) = field.error_parts().unwrap();
                    assert_eq!((code, message), (38, "not implemented"));
                }
                Kind::Handle => assert_eq!(field.as_handle().unwrap(), 0xdead_beef),
                Kind::Buffer => {
                    let buffer = field.as_buffer().unwrap();
                    assert_eq!(buffer.len, 8192);
                }
            }
            seen += 1;
        }
        assert_eq!(seen, 16, "every kind should be exercised");
    }

    #[test]
    fn unknown_kinds_are_skipped() {
        // A field with kind 200 (unknown) followed by a known `u32`.
        let mut body = Vec::new();
        body.extend_from_slice(&(200u32 | (1 << 8)).to_le_bytes());
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(&[9, 9, 9, 9]);
        let mut encoder = Encoder::new();
        encoder.u32(7, 1234).unwrap();
        body.extend_from_slice(encoder.as_bytes());

        let mut decoder = Decoder::new(&body);
        let field = decoder.next().unwrap().unwrap();
        assert_eq!(field.kind, Kind::U32);
        assert_eq!(field.id, 7);
        assert_eq!(field.as_u32().unwrap(), 1234);
        assert!(decoder.next().unwrap().is_none());
    }

    #[test]
    fn limits_are_enforced() {
        // Too many handles.
        let mut parcel = sample_parcel();
        parcel.handles = vec![0; MAX_HANDLES + 1];
        let mut bytes = Vec::new();
        assert_eq!(parcel.encode(&mut bytes), Err(Error::TooLarge));

        // Body over the limit.
        let mut parcel = sample_parcel();
        parcel.body = vec![0; MAX_BODY_BYTES + 1];
        assert_eq!(parcel.encode(&mut bytes), Err(Error::TooLarge));

        // Trailing bytes after a valid parcel.
        let parcel = sample_parcel();
        parcel.encode(&mut bytes).unwrap();
        bytes.push(0);
        assert_eq!(Parcel::decode(&bytes), Err(Error::TrailingBytes));

        // Bad version.
        let mut bytes2 = Vec::new();
        parcel.encode(&mut bytes2).unwrap();
        bytes2[0] = 99;
        assert_eq!(Parcel::decode(&bytes2), Err(Error::BadVersion));
    }

    /// A tiny deterministic PRNG (xorshift64*), so the fuzz test needs no deps.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
    }

    #[test]
    fn fuzz_decode_never_panics() {
        const ITERATIONS: usize = 1_000_000;
        let mut rng = Rng(0x1234_5678_9abc_def0);
        let valid = {
            let mut bytes = Vec::new();
            sample_parcel().encode(&mut bytes).unwrap();
            bytes
        };

        for i in 0..ITERATIONS {
            // Start from random bytes or a mutated valid parcel.
            let mut bytes = if i % 2 == 0 {
                let len = (rng.next() % 256) as usize;
                let mut v = vec![0u8; len];
                for b in v.iter_mut() {
                    *b = rng.next() as u8;
                }
                v
            } else {
                valid.clone()
            };
            let mutations = rng.next() % 8;
            for _ in 0..mutations {
                if bytes.is_empty() {
                    break;
                }
                let at = (rng.next() as usize) % bytes.len();
                bytes[at] = rng.next() as u8;
            }
            // Truncate sometimes.
            if rng.next() % 4 == 0 && !bytes.is_empty() {
                let keep = (rng.next() as usize) % bytes.len();
                bytes.truncate(keep);
            }

            // The contract: no panic, ever. Contents may be Ok or Err.
            let _ = Parcel::decode(&bytes);
            if let Ok(parcel) = Parcel::decode(&bytes) {
                let mut decoder = Decoder::new(&parcel.body);
                while let Ok(Some(field)) = decoder.next() {
                    let _ = field.as_u64();
                    let _ = field.as_str();
                    let _ = field.nested(0);
                    let _ = field.error_parts();
                }
            }
        }
    }
}

#[cfg(test)]
impl<'a> Decoder<'a> {
    /// Count the known fields in the remaining input (test helper).
    fn count(&mut self) -> usize {
        let mut n = 0;
        while let Ok(Some(_)) = self.next() {
            n += 1;
        }
        n
    }
}
