//! Binary store encoding: a magic header, length-prefixed entries, CRC-32.
//!
//! ```text
//! store   := magic entries crc32
//! magic   := "REGD"
//! entries := entry*
//! entry   := path_len:u16 path:u8[path_len] tag:u8 value
//! value   :=
//!   tag 0: bool:u8 (0 or 1)
//!   tag 1: i64:8 bytes
//!   tag 2: u64:8 bytes
//!   tag 3: len:u32 bytes:u8[len]   (UTF-8)
//!   tag 4: len:u32 bytes:u8[len]
//! crc32   := CRC-32/ISO-HDLC of everything before it, little-endian
//! ```
//!
//! There is no explicit version field (the plan calls the format
//! versionless): a future incompatible format gets a different magic. The
//! format is deterministic — entries follow `BTreeMap` order — so encoded
//! bytes are comparable across runs.
//!
//! `decode` is the untrusted boundary. It verifies the CRC before parsing,
//! then re-validates every path and limit, and allocates nothing from a
//! length it has not first checked against the remaining input and
//! [`crate::MAX_VALUE_LEN`].

use alloc::string::String;
use alloc::vec::Vec;

use crate::path::validate_path;
use crate::store::Store;
use crate::value::Value;
use crate::{MAX_PATH_LEN, MAX_STORE_BYTES, MAX_VALUE_LEN};

/// Magic bytes at the start of every encoded store.
const MAGIC: [u8; 4] = *b"REGD";
/// Trailer size: one CRC-32.
const CRC_SIZE: usize = 4;

/// Hard bound on encoded input.
///
/// An entry's framing (length prefixes and tag) adds at most 7 bytes to its
/// logical `path + value` size, and every entry has a logical size of at
/// least one byte (its path), so any legal store encodes to at most
/// `8 * MAX_STORE_BYTES` bytes plus the magic and CRC. `decode` rejects
/// anything larger up front.
pub const MAX_ENCODED_LEN: usize = 8 * MAX_STORE_BYTES + MAGIC.len() + CRC_SIZE;

/// Why an encoded store could not be decoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecodeError {
    /// Input is shorter than the magic and CRC framing.
    TooShort,
    /// The magic header is not `REGD`.
    BadMagic,
    /// The CRC-32 trailer does not match the payload.
    BadCrc,
    /// Input is longer than [`MAX_ENCODED_LEN`].
    TooLarge,
    /// Entry framing is malformed: unknown tag, bad boolean, truncated or
    /// non-UTF-8 string, or a length running past the end.
    Malformed,
    /// A path fails [`validate_path`].
    BadPath,
    /// The same path appears twice.
    Duplicate,
}

impl DecodeError {
    /// A short, human-readable explanation (friendly-errors convention).
    pub const fn message(self) -> &'static str {
        match self {
            DecodeError::TooShort => "encoded store is too short to be complete",
            DecodeError::BadMagic => "encoded store has no regd magic header",
            DecodeError::BadCrc => "encoded store failed its CRC check",
            DecodeError::TooLarge => "encoded store exceeds a regd size limit",
            DecodeError::Malformed => "encoded store entry is malformed",
            DecodeError::BadPath => "encoded store contains an invalid path",
            DecodeError::Duplicate => "encoded store contains a path twice",
        }
    }
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

/// Serialises a valid store. Cannot fail: every entry passed through
/// [`Store::set`] or `decode`, so lengths already satisfy the limits.
pub fn encode(store: &Store) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    for (path, value) in store.iter_raw() {
        debug_assert!(path.len() <= MAX_PATH_LEN);
        out.extend_from_slice(&(path.len() as u16).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        encode_value(&mut out, value);
    }
    out.extend_from_slice(&crc32(&out).to_le_bytes());
    out
}

fn encode_value(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Bool(flag) => {
            out.push(0);
            out.push(u8::from(*flag));
        }
        Value::I64(number) => {
            out.push(1);
            out.extend_from_slice(&number.to_le_bytes());
        }
        Value::U64(number) => {
            out.push(2);
            out.extend_from_slice(&number.to_le_bytes());
        }
        Value::Str(text) => {
            out.push(3);
            push_blob(out, text.as_bytes());
        }
        Value::Bytes(bytes) => {
            out.push(4);
            push_blob(out, bytes);
        }
    }
}

fn push_blob(out: &mut Vec<u8>, bytes: &[u8]) {
    debug_assert!(bytes.len() <= MAX_VALUE_LEN);
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// Parses an encoded store, re-validating every path and limit.
///
/// # Errors
///
/// A [`DecodeError`] for any truncated, corrupt, oversized, malformed or
/// duplicate content. The function never panics and never treats a declared
/// length as trustworthy before checking it against the remaining input.
pub fn decode(data: &[u8]) -> Result<Store, DecodeError> {
    if data.len() < MAGIC.len() + CRC_SIZE {
        return Err(DecodeError::TooShort);
    }
    if data.len() > MAX_ENCODED_LEN {
        return Err(DecodeError::TooLarge);
    }
    let (payload, trailer) = data.split_at(data.len() - CRC_SIZE);
    if !payload.starts_with(&MAGIC) {
        return Err(DecodeError::BadMagic);
    }
    let expected = u32::from_le_bytes(trailer.try_into().map_err(|_| DecodeError::TooShort)?);
    if crc32(payload) != expected {
        return Err(DecodeError::BadCrc);
    }

    let (_, body) = payload.split_at(MAGIC.len());
    let mut reader = Reader::new(body);
    let mut store = Store::new();
    let mut total = 0usize;
    while reader.remaining() > 0 {
        let path_len = reader.u16()? as usize;
        if path_len == 0 || path_len > MAX_PATH_LEN {
            return Err(DecodeError::BadPath);
        }
        let path_bytes = reader.take(path_len)?;
        let path = core::str::from_utf8(path_bytes).map_err(|_| DecodeError::BadPath)?;
        validate_path(path).map_err(|_| DecodeError::BadPath)?;
        let value = decode_value(&mut reader)?;
        total = total
            .checked_add(path_len + value.size_bytes())
            .ok_or(DecodeError::TooLarge)?;
        if total > MAX_STORE_BYTES {
            return Err(DecodeError::TooLarge);
        }
        if !store.insert_decoded(String::from(path), value) {
            return Err(DecodeError::Duplicate);
        }
    }
    Ok(store)
}

fn decode_value(reader: &mut Reader<'_>) -> Result<Value, DecodeError> {
    match reader.u8()? {
        0 => match reader.u8()? {
            0 => Ok(Value::Bool(false)),
            1 => Ok(Value::Bool(true)),
            _ => Err(DecodeError::Malformed),
        },
        1 => Ok(Value::I64(i64::from_le_bytes(reader.array()?))),
        2 => Ok(Value::U64(u64::from_le_bytes(reader.array()?))),
        3 => {
            let bytes = decode_blob(reader)?;
            let text = core::str::from_utf8(bytes).map_err(|_| DecodeError::Malformed)?;
            Ok(Value::Str(String::from(text)))
        }
        4 => Ok(Value::Bytes(decode_blob(reader)?.to_vec())),
        _ => Err(DecodeError::Malformed),
    }
}

/// Reads one length-prefixed blob. The length is checked against the value
/// limit and the remaining input before any allocation happens.
fn decode_blob<'a>(reader: &mut Reader<'a>) -> Result<&'a [u8], DecodeError> {
    let len = reader.u32()? as usize;
    if len > MAX_VALUE_LEN {
        return Err(DecodeError::TooLarge);
    }
    reader.take(len)
}

/// A cursor that turns "past the end" into [`DecodeError::Malformed`] instead
/// of a panic.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(count).ok_or(DecodeError::Malformed)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(DecodeError::Malformed)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        self.take(1)?.first().copied().ok_or(DecodeError::Malformed)
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    /// Reads a fixed-size array; the `Result` only ever fails when the input
    /// ends early, which is exactly a malformed entry.
    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        self.take(N)?.try_into().map_err(|_| DecodeError::Malformed)
    }
}

/// CRC-32/ISO-HDLC (the zlib/PNG polynomial, reflected).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        crc = (crc >> 8) ^ CRC32_TABLE[(crc & 0xFF) as usize];
    }
    !crc
}

const CRC32_TABLE: [u32; 256] = build_crc32_table();

const fn build_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_answer() {
        // The standard CRC-32/ISO-HDLC check value.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }
}
