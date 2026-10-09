//! TLV field encoding and decoding.

use alloc::vec::Vec;

use crate::{Buffer, Error, Object, MAX_BODY_BYTES, MAX_DEPTH, MAX_FIELDS, MAX_OBJECTS};

/// Payload size of a `Handle` field: the `u32` index of a channel end.
const HANDLE_FIELD_SIZE: usize = 4;
/// Payload size of a `Buffer` field: `u32` index, `u64` offset, `u64` len.
const BUFFER_FIELD_SIZE: usize = 20;

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

    /// Write a field whose payload was encoded elsewhere (e.g. a generated
    /// nested record helper). `kind` must match the payload's encoding.
    pub fn raw(&mut self, kind: Kind, id: u16, payload: &[u8]) -> Result<(), Error> {
        self.push(kind, id, payload)
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
    ///
    /// A reader stops the message at its first NUL byte (see
    /// [`Encoder::error_detail`]), so a message should not contain one.
    pub fn error(&mut self, id: u16, code: u32, message: &str) -> Result<(), Error> {
        let mut payload = Vec::with_capacity(4 + message.len());
        payload.extend_from_slice(&code.to_le_bytes());
        payload.extend_from_slice(message.as_bytes());
        self.push(Kind::Error, id, &payload)
    }

    /// A structured error with a detail record: the code, the message, a NUL
    /// byte, then `detail`'s fields. A reader that only knows
    /// [`Field::error_parts`] still sees the code and the message. The
    /// message must not contain a NUL (`BadString`), or the detail would
    /// start inside it.
    pub fn error_detail(
        &mut self,
        id: u16,
        code: u32,
        message: &str,
        detail: &Encoder,
    ) -> Result<(), Error> {
        if message.as_bytes().contains(&0) {
            return Err(Error::BadString);
        }
        let mut payload = Vec::with_capacity(5 + message.len() + detail.as_bytes().len());
        payload.extend_from_slice(&code.to_le_bytes());
        payload.extend_from_slice(message.as_bytes());
        payload.push(0);
        payload.extend_from_slice(detail.as_bytes());
        self.push(Kind::Error, id, &payload)
    }

    /// A channel end: `handle` (the sender's number) joins the parcel's
    /// `objects` list and the field carries its index.
    pub fn channel(
        &mut self,
        id: u16,
        handle: u64,
        objects: &mut Vec<Object>,
    ) -> Result<(), Error> {
        let index = push_object(objects, Object::Channel(handle))?;
        self.push(Kind::Handle, id, &index.to_le_bytes())
    }

    /// A shared buffer: `buffer.handle` joins the parcel's `objects` list and
    /// the field carries its index with the byte range.
    pub fn buffer(
        &mut self,
        id: u16,
        buffer: &Buffer,
        objects: &mut Vec<Object>,
    ) -> Result<(), Error> {
        let index = push_object(objects, Object::Buffer(buffer.handle))?;
        let mut payload = [0u8; BUFFER_FIELD_SIZE];
        payload[0..4].copy_from_slice(&index.to_le_bytes());
        payload[4..12].copy_from_slice(&buffer.offset.to_le_bytes());
        payload[12..20].copy_from_slice(&buffer.len.to_le_bytes());
        self.push(Kind::Buffer, id, &payload)
    }
}

/// Append `object` to a parcel's object list and return its index.
fn push_object(objects: &mut Vec<Object>, object: Object) -> Result<u32, Error> {
    if objects.len() >= MAX_OBJECTS {
        return Err(Error::TooLarge);
    }
    objects.push(object);
    Ok((objects.len() - 1) as u32)
}

/// Take the object at `index` out of `objects` under the index rule
/// (`docs/messenger-core-plan.md` 2.3): an object field's index must be its
/// position in the declared order, which `next` counts, so a repeated,
/// skipped or out-of-range index is refused and every object is claimed by
/// exactly one field. The caller advances `next` once the kind matches too.
fn claim(objects: &[Object], next: usize, index: u32) -> Result<Object, Error> {
    if index as usize != next {
        return Err(Error::BadObjectIndex);
    }
    objects.get(next).copied().ok_or(Error::BadObjectIndex)
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
    /// Split an `Error` field into `(code, message)`. The message ends at
    /// the first NUL byte, where an [`Encoder::error_detail`] record starts.
    pub fn error_parts(&self) -> Result<(u32, &'a str), Error> {
        if self.payload.len() < 4 {
            return Err(Error::BadValue);
        }
        let code = read_u32(self.payload, 0)?;
        let text = &self.payload[4..];
        let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
        let message = core::str::from_utf8(&text[..end]).map_err(|_| Error::BadString)?;
        Ok((code, message))
    }

    /// The detail record of an `Error` field written by
    /// [`Encoder::error_detail`], or `None` when it has none.
    pub fn error_detail(&self) -> Option<Decoder<'a>> {
        let text = self.payload.get(4..)?;
        let nul = text.iter().position(|&b| b == 0)?;
        Some(Decoder {
            buf: &text[nul + 1..],
            pos: 0,
        })
    }
    /// The index a `Handle` field carries into the object list.
    pub fn object_index(&self) -> Result<u32, Error> {
        if self.payload.len() != HANDLE_FIELD_SIZE {
            return Err(Error::BadValue);
        }
        read_u32(self.payload, 0)
    }

    /// A `Buffer` field's `(index, offset, len)`.
    pub fn buffer_parts(&self) -> Result<(u32, u64, u64), Error> {
        if self.payload.len() != BUFFER_FIELD_SIZE {
            return Err(Error::BadValue);
        }
        Ok((
            read_u32(self.payload, 0)?,
            read_u64(self.payload, 4)?,
            read_u64(self.payload, 12)?,
        ))
    }

    /// Claim the channel end this `Handle` field names in `objects` (the
    /// receiver's list, handles already installed): the field's index must
    /// be `*next`, the declared position the caller counts, and the entry
    /// must be a channel. Returns the handle and advances `next`.
    pub fn claim_channel(&self, objects: &[Object], next: &mut usize) -> Result<u64, Error> {
        match claim(objects, *next, self.object_index()?)? {
            Object::Channel(handle) => {
                *next += 1;
                Ok(handle)
            }
            Object::Buffer(_) => Err(Error::BadObjectIndex),
        }
    }

    /// Claim the shared buffer this `Buffer` field names in `objects`, like
    /// [`Field::claim_channel`]; the range is data the caller checks against
    /// the mapped size.
    pub fn claim_buffer(&self, objects: &[Object], next: &mut usize) -> Result<Buffer, Error> {
        let (index, offset, len) = self.buffer_parts()?;
        match claim(objects, *next, index)? {
            Object::Buffer(handle) => {
                *next += 1;
                Ok(Buffer {
                    handle,
                    offset,
                    len,
                })
            }
            Object::Channel(_) => Err(Error::BadObjectIndex),
        }
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
    ///
    /// Not `Iterator::next`: this is fallible (a malformed parcel is an
    /// `Error`, not a panic or a silent stop), so it keeps its own name
    /// rather than implementing the trait.
    #[allow(clippy::should_implement_trait)]
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
pub(crate) fn read_u16(buf: &[u8], at: usize) -> Result<u16, Error> {
    let bytes = buf.get(at..at + 2).ok_or(Error::Truncated)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// Read a little-endian `u32`, or `Truncated`.
pub(crate) fn read_u32(buf: &[u8], at: usize) -> Result<u32, Error> {
    let bytes = buf.get(at..at + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Read a little-endian `u64`, or `Truncated`.
pub(crate) fn read_u64(buf: &[u8], at: usize) -> Result<u64, Error> {
    let bytes = buf.get(at..at + 8).ok_or(Error::Truncated)?;
    let mut array = [0u8; 8];
    array.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(array))
}

#[cfg(test)]
impl<'a> Decoder<'a> {
    /// Count the known fields in the remaining input (test helper).
    pub(crate) fn count(&mut self) -> usize {
        let mut n = 0;
        while let Ok(Some(_)) = self.next() {
            n += 1;
        }
        n
    }
}
