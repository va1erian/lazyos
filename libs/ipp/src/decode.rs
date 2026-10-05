//! Reading a [`Message`] from untrusted bytes.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{tag, Attribute, Group, Message, Value, MAX_ATTRIBUTES, MAX_DEPTH};

/// Why bytes are not an IPP message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The bytes end inside the header, a field or before the end tag.
    Truncated,
    /// A reserved delimiter (`0x00`) or the extension tag (`0x7F`).
    BadTag(u8),
    /// A value's length does not fit its tag.
    BadLength(u8),
    /// An attribute before the first group, or an additional value with no
    /// attribute to add to.
    Orphan,
    /// A collection that is not `begCollection`, members, `endCollection`.
    BadCollection,
    /// Collections nested deeper than [`MAX_DEPTH`].
    TooDeep,
    /// More than [`MAX_ATTRIBUTES`] attributes.
    TooMany,
}

impl Message {
    /// Decodes a message, returning it and where the data after its end tag
    /// starts (the document of a request, nothing in a response).
    pub fn decode(bytes: &[u8]) -> Result<(Message, usize), DecodeError> {
        let mut r = Reader {
            bytes,
            at: 0,
            attributes: 0,
        };
        let head = r.take(8)?;
        let mut message = Message {
            version: (head[0], head[1]),
            code: u16::from_be_bytes([head[2], head[3]]),
            request_id: u32::from_be_bytes([head[4], head[5], head[6], head[7]]),
            groups: Vec::new(),
        };
        loop {
            let t = r.byte()?;
            if t == tag::END {
                return Ok((message, r.at));
            }
            if t == 0 || t == tag::EXTENSION {
                return Err(DecodeError::BadTag(t));
            }
            if t <= tag::MAX_DELIMITER {
                message.groups.push(Group::new(t));
                continue;
            }
            let group = message.groups.last_mut().ok_or(DecodeError::Orphan)?;
            let name = r.field()?;
            let value = r.value(t, 1)?;
            if name.is_empty() {
                let last = group.attributes.last_mut().ok_or(DecodeError::Orphan)?;
                last.values.push(value);
            } else {
                r.count()?;
                group.attributes.push(Attribute {
                    name: text(name),
                    values: alloc::vec![value],
                });
            }
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    attributes: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.at.checked_add(n).ok_or(DecodeError::Truncated)?;
        let out = self.bytes.get(self.at..end).ok_or(DecodeError::Truncated)?;
        self.at = end;
        Ok(out)
    }

    fn byte(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    /// A 2-byte length and that many bytes.
    fn field(&mut self) -> Result<&'a [u8], DecodeError> {
        let n = self.take(2)?;
        self.take(usize::from(u16::from_be_bytes([n[0], n[1]])))
    }

    fn count(&mut self) -> Result<(), DecodeError> {
        self.attributes += 1;
        if self.attributes > MAX_ATTRIBUTES {
            return Err(DecodeError::TooMany);
        }
        Ok(())
    }

    /// The value of tag `t` (its name already read), `depth` collections
    /// deep if it is one.
    fn value(&mut self, t: u8, depth: usize) -> Result<Value, DecodeError> {
        let data = self.field()?;
        if t == tag::BEG_COLLECTION {
            return self.collection(depth);
        }
        scalar(t, data)
    }

    /// A collection's members, up to and including `endCollection`.
    fn collection(&mut self, depth: usize) -> Result<Value, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError::TooDeep);
        }
        let mut members: Vec<Attribute> = Vec::new();
        loop {
            let t = self.byte()?;
            match t {
                tag::END_COLLECTION => {
                    self.field()?;
                    self.field()?;
                    if members.last().is_some_and(|m| m.values.is_empty()) {
                        return Err(DecodeError::BadCollection);
                    }
                    return Ok(Value::Collection(members));
                }
                tag::MEMBER_ATTR_NAME => {
                    self.field()?;
                    let name = self.field()?;
                    if name.is_empty() || members.last().is_some_and(|m| m.values.is_empty()) {
                        return Err(DecodeError::BadCollection);
                    }
                    self.count()?;
                    members.push(Attribute {
                        name: text(name),
                        values: Vec::new(),
                    });
                }
                0..=tag::MAX_DELIMITER | tag::EXTENSION => return Err(DecodeError::BadCollection),
                _ => {
                    self.field()?;
                    let value = self.value(t, depth + 1)?;
                    let member = members.last_mut().ok_or(DecodeError::BadCollection)?;
                    member.values.push(value);
                }
            }
        }
    }
}

/// A value that is not a collection.
fn scalar(t: u8, data: &[u8]) -> Result<Value, DecodeError> {
    let bad = DecodeError::BadLength(t);
    let int = |d: &[u8]| -> Result<i32, DecodeError> {
        Ok(i32::from_be_bytes(d.try_into().map_err(|_| bad)?))
    };
    Ok(match t {
        tag::INTEGER => Value::Integer(int(data)?),
        tag::ENUM => Value::Enum(int(data)?),
        tag::BOOLEAN => match data {
            [0] => Value::Boolean(false),
            [1] => Value::Boolean(true),
            _ => return Err(bad),
        },
        tag::RESOLUTION if data.len() == 9 => Value::Resolution {
            x: int(&data[..4])?,
            y: int(&data[4..8])?,
            units: data[8],
        },
        tag::RANGE_OF_INTEGER if data.len() == 8 => Value::Range {
            lower: int(&data[..4])?,
            upper: int(&data[4..])?,
        },
        tag::RESOLUTION | tag::RANGE_OF_INTEGER => return Err(bad),
        tag::TEXT_WITH_LANGUAGE | tag::NAME_WITH_LANGUAGE => {
            let mut inner = Reader {
                bytes: data,
                at: 0,
                attributes: 0,
            };
            let language = inner.field().map_err(|_| bad)?;
            let body = inner.field().map_err(|_| bad)?;
            if inner.at != data.len() {
                return Err(bad);
            }
            Value::WithLanguage {
                tag: t,
                language: text(language),
                text: text(body),
            }
        }
        _ if tag::is_string(t) => Value::String {
            tag: t,
            value: text(data),
        },
        _ if tag::is_out_of_band(t) => Value::OutOfBand(t),
        tag::END_COLLECTION | tag::MEMBER_ATTR_NAME => return Err(DecodeError::BadCollection),
        _ => Value::Other {
            tag: t,
            data: data.to_vec(),
        },
    })
}

/// IPP strings are UTF-8; anything else is shown with replacement characters
/// rather than refused, since it is only ever displayed.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
