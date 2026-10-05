//! Writing a [`Message`] in the RFC 8010 binary encoding.

use alloc::vec::Vec;

use crate::{tag, Attribute, Message, Value};

/// Why a message could not be encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// A name or value is longer than the 65535 bytes a length field holds.
    TooLong,
    /// An attribute has no value (the wire has no way to say so).
    Empty,
    /// A group's delimiter tag is not one (`0x01..=0x0F` without `0x03`).
    BadGroup,
}

impl Message {
    /// The message in the RFC 8010 encoding: header, groups, end tag. The
    /// document data of a Print-Job follows these bytes on the wire.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let mut out = Vec::with_capacity(256);
        out.extend_from_slice(&[self.version.0, self.version.1]);
        out.extend_from_slice(&self.code.to_be_bytes());
        out.extend_from_slice(&self.request_id.to_be_bytes());
        for group in &self.groups {
            if group.tag == 0 || group.tag == tag::END || group.tag > tag::MAX_DELIMITER {
                return Err(EncodeError::BadGroup);
            }
            out.push(group.tag);
            for attribute in &group.attributes {
                attr(&mut out, attribute)?;
            }
        }
        out.push(tag::END);
        Ok(out)
    }
}

/// An attribute: its first value carries the name, the rest an empty one.
fn attr(out: &mut Vec<u8>, attribute: &Attribute) -> Result<(), EncodeError> {
    if attribute.values.is_empty() {
        return Err(EncodeError::Empty);
    }
    for (i, value) in attribute.values.iter().enumerate() {
        let name = if i == 0 {
            attribute.name.as_bytes()
        } else {
            &[]
        };
        one(out, name, value)?;
    }
    Ok(())
}

/// One value with `name` (empty for an additional value or a member value).
fn one(out: &mut Vec<u8>, name: &[u8], value: &Value) -> Result<(), EncodeError> {
    out.push(value.tag());
    field(out, name)?;
    match value {
        Value::Integer(n) | Value::Enum(n) => field(out, &n.to_be_bytes()),
        Value::Boolean(b) => field(out, &[u8::from(*b)]),
        Value::Resolution { x, y, units } => {
            let mut data = [0u8; 9];
            data[..4].copy_from_slice(&x.to_be_bytes());
            data[4..8].copy_from_slice(&y.to_be_bytes());
            data[8] = *units;
            field(out, &data)
        }
        Value::Range { lower, upper } => {
            let mut data = [0u8; 8];
            data[..4].copy_from_slice(&lower.to_be_bytes());
            data[4..].copy_from_slice(&upper.to_be_bytes());
            field(out, &data)
        }
        Value::WithLanguage { language, text, .. } => {
            let (language, text) = (language.as_bytes(), text.as_bytes());
            let total = 4 + language.len() + text.len();
            len(out, total)?;
            field(out, language)?;
            field(out, text)
        }
        Value::String { value, .. } => field(out, value.as_bytes()),
        Value::Collection(members) => {
            field(out, &[])?;
            for member in members {
                out.push(tag::MEMBER_ATTR_NAME);
                field(out, &[])?;
                field(out, member.name.as_bytes())?;
                if member.values.is_empty() {
                    return Err(EncodeError::Empty);
                }
                for value in &member.values {
                    one(out, &[], value)?;
                }
            }
            out.push(tag::END_COLLECTION);
            field(out, &[])?;
            field(out, &[])
        }
        Value::OutOfBand(_) => field(out, &[]),
        Value::Other { data, .. } => field(out, data),
    }
}

/// A 2-byte length and the bytes.
fn field(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), EncodeError> {
    len(out, bytes.len())?;
    out.extend_from_slice(bytes);
    Ok(())
}

fn len(out: &mut Vec<u8>, n: usize) -> Result<(), EncodeError> {
    let n = u16::try_from(n).map_err(|_| EncodeError::TooLong)?;
    out.extend_from_slice(&n.to_be_bytes());
    Ok(())
}
