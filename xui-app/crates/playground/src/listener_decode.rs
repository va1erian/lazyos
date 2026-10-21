//! Decoding the bytes of a broker event: by the declared type when the
//! schema has one for the topic, as bytes otherwise. The payload is
//! "opaque" to the broker, so anything can arrive here: this never panics and
//! never allocates more than a bounded amount for what it keeps.

use rhai::Dynamic;
use rhai_lazy::msg::{codec, schema, topics};

use crate::show;

/// Payloads longer than this are not decoded (the broker's own cap is 8 KiB).
pub const MAX_DECODE: usize = 16 * 1024;
/// Bytes of a payload an event keeps.
pub const MAX_KEPT: usize = 8 * 1024;

/// What an event carried.
#[derive(Debug, Clone)]
pub enum Body {
    /// Decoded with the declared type `ty`.
    Decoded { ty: String, value: Dynamic },
    /// A topic nothing declares: the raw payload.
    Bytes { bytes: Vec<u8>, len: usize },
    /// A declared topic whose payload did not decode as its type.
    Undecodable {
        ty: String,
        error: String,
        bytes: Vec<u8>,
        len: usize,
    },
}

impl Body {
    /// The payload as a Rhai value: the decoded value or a blob.
    pub fn value(&self) -> Dynamic {
        match self {
            Body::Decoded { value, .. } => value.clone(),
            Body::Bytes { bytes, .. } | Body::Undecodable { bytes, .. } => {
                Dynamic::from_blob(bytes.clone())
            }
        }
    }

    /// The declared payload type, if the topic has one.
    pub fn declared(&self) -> Option<&str> {
        match self {
            Body::Decoded { ty, .. } | Body::Undecodable { ty, .. } => Some(ty),
            Body::Bytes { .. } => None,
        }
    }

    pub fn is_decoded(&self) -> bool {
        matches!(self, Body::Decoded { .. })
    }

    /// A short tag for the list: the type, `bytes`, or `undecodable`.
    pub fn kind_text(&self) -> String {
        match self {
            Body::Decoded { ty, .. } => ty.clone(),
            Body::Bytes { len, .. } => format!("bytes({len})"),
            Body::Undecodable { ty, .. } => format!("{ty}?"),
        }
    }

    /// The body as an indented tree, or the error and the bytes.
    pub fn tree(&self) -> String {
        match self {
            Body::Decoded { value, .. } => show::tree(value),
            Body::Bytes { bytes, len } => show::blob_line(&bytes[..bytes.len().min(*len)]),
            Body::Undecodable {
                error, bytes, ty, ..
            } => format!("not a {ty}: {error}\n{}", show::blob_line(bytes)),
        }
    }
}

fn kept(bytes: &[u8]) -> Vec<u8> {
    bytes[..bytes.len().min(MAX_KEPT)].to_vec()
}

/// Decode the payload of an event on `topic`. `wrapped` is the payload as the
/// broker carries it (inside the platform's envelope); anything else is
/// handed through as raw bytes.
pub fn decode(topic: &str, wrapped: &[u8]) -> Body {
    let len = wrapped.len();
    let Some((interface, decl)) = schema::declared_topic(topic) else {
        return Body::Bytes {
            bytes: kept(&topics::unwrap(&wrapped[..len.min(MAX_DECODE)])),
            len,
        };
    };
    let ty = decl.payload.to_string();
    if len > MAX_DECODE {
        return Body::Undecodable {
            ty,
            error: format!("{len} bytes is too long to decode (limit {MAX_DECODE})"),
            bytes: kept(wrapped),
            len,
        };
    }
    let bytes = topics::unwrap(wrapped);
    match codec::decode_payload(interface, decl.payload, &bytes) {
        Ok(value) => Body::Decoded { ty, value },
        Err(error) => Body::Undecodable {
            ty,
            error,
            bytes: kept(&bytes),
            len,
        },
    }
}
