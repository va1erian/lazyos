//! IPP/2.0 messages (RFC 8010) for LazyOS's print client
//! (docs/printing-plan.md, stage P1).
//!
//! A printer's reply is untrusted input from the network, so everything that
//! reads one lives here, pure and host tested:
//!
//! * [`Message`]: a request or response, its attribute groups and values,
//!   with [`Message::encode`] and [`Message::decode`]. Decoding is strict and
//!   bounded ([`MAX_DEPTH`], [`MAX_ATTRIBUTES`]): a message that breaks the
//!   format is an error, never a guess, and nothing it says sizes an
//!   allocation beyond the bytes actually received.
//! * [`request`]: the operations a print client sends (Get-Printer-Attributes,
//!   Validate-Job, Print-Job, Get-Job-Attributes, Cancel-Job) with the
//!   operation attributes RFC 8011 requires, in its order.
//! * [`http`] (feature `std`): the HTTP/1.1 transport, a chunked `POST` of
//!   `application/ipp` followed by the document, and a bounded reply reader.
//!
//! Values keep the wire's own tag, so a reply re-encodes byte for byte and a
//! value of a kind this crate does not interpret survives as
//! [`Value::Other`].

#![cfg_attr(not(any(test, feature = "std", feature = "fuzz")), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

mod decode;
mod encode;
pub mod request;
pub mod tag;
pub mod uri;

#[cfg(feature = "std")]
pub mod http;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;

#[cfg(test)]
mod tests;

use alloc::string::String;
use alloc::vec::Vec;

pub use decode::DecodeError;
pub use encode::EncodeError;

/// Deepest nesting of collections a decoded message may have.
pub const MAX_DEPTH: usize = 8;
/// Most attributes (collection members included) a decoded message may have.
pub const MAX_ATTRIBUTES: usize = 4096;

/// One attribute value, by its wire tag ([`tag`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// `integer`.
    Integer(i32),
    /// `boolean`.
    Boolean(bool),
    /// `enum`: an integer from an attribute's own table (`job-state`, ...).
    Enum(i32),
    /// `resolution`: cross-feed, feed, units (3 dots per inch, 4 per cm).
    Resolution { x: i32, y: i32, units: u8 },
    /// `rangeOfInteger`.
    Range { lower: i32, upper: i32 },
    /// `textWithLanguage` or `nameWithLanguage` (`tag` says which).
    WithLanguage {
        tag: u8,
        language: String,
        text: String,
    },
    /// A string kind: `textWithoutLanguage`, `nameWithoutLanguage`,
    /// `keyword`, `uri`, `uriScheme`, `charset`, `naturalLanguage` or
    /// `mimeMediaType` (`tag` says which). IPP strings are UTF-8 (RFC 8011
    /// 5.1); invalid bytes are replaced on decode.
    String { tag: u8, value: String },
    /// `begCollection`: members in wire order.
    Collection(Vec<Attribute>),
    /// An out-of-band value: `unsupported`, `unknown` or `no-value`.
    OutOfBand(u8),
    /// Any other value tag (`octetString`, `dateTime`, ...), kept as bytes.
    Other { tag: u8, data: Vec<u8> },
}

impl Value {
    /// A `keyword` value.
    pub fn keyword(value: &str) -> Value {
        Value::string(tag::KEYWORD, value)
    }

    /// A `nameWithoutLanguage` value.
    pub fn name(value: &str) -> Value {
        Value::string(tag::NAME, value)
    }

    /// A `uri` value.
    pub fn uri(value: &str) -> Value {
        Value::string(tag::URI, value)
    }

    /// A `mimeMediaType` value.
    pub fn mime(value: &str) -> Value {
        Value::string(tag::MIME_MEDIA_TYPE, value)
    }

    /// A string value of kind `tag`.
    pub fn string(tag: u8, value: &str) -> Value {
        Value::String {
            tag,
            value: String::from(value),
        }
    }

    /// The value's wire tag.
    pub fn tag(&self) -> u8 {
        match self {
            Value::Integer(_) => tag::INTEGER,
            Value::Boolean(_) => tag::BOOLEAN,
            Value::Enum(_) => tag::ENUM,
            Value::Resolution { .. } => tag::RESOLUTION,
            Value::Range { .. } => tag::RANGE_OF_INTEGER,
            Value::WithLanguage { tag, .. } | Value::String { tag, .. } => *tag,
            Value::Collection(_) => tag::BEG_COLLECTION,
            Value::OutOfBand(tag) | Value::Other { tag, .. } => *tag,
        }
    }

    /// The number of an `integer` or `enum`.
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Value::Integer(n) | Value::Enum(n) => Some(*n),
            _ => None,
        }
    }

    /// The text of any string kind, with or without a language.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String { value, .. } => Some(value),
            Value::WithLanguage { text, .. } => Some(text),
            _ => None,
        }
    }

    /// The members of a collection.
    pub fn as_collection(&self) -> Option<&[Attribute]> {
        match self {
            Value::Collection(members) => Some(members),
            _ => None,
        }
    }
}

/// A named attribute and its values (at least one on the wire).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub name: String,
    pub values: Vec<Value>,
}

impl Attribute {
    /// An attribute with one value.
    pub fn new(name: &str, value: Value) -> Attribute {
        Attribute {
            name: String::from(name),
            values: alloc::vec![value],
        }
    }

    /// An attribute with several values (a `1setOf`).
    pub fn set(name: &str, values: Vec<Value>) -> Attribute {
        Attribute {
            name: String::from(name),
            values,
        }
    }

    /// The first value.
    pub fn first(&self) -> Option<&Value> {
        self.values.first()
    }
}

/// An attribute group: its delimiter tag ([`tag::OPERATION`], ...) and
/// attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub tag: u8,
    pub attributes: Vec<Attribute>,
}

impl Group {
    /// An empty group with delimiter `tag`.
    pub fn new(tag: u8) -> Group {
        Group {
            tag,
            attributes: Vec::new(),
        }
    }

    /// The attribute called `name`.
    pub fn get(&self, name: &str) -> Option<&Attribute> {
        self.attributes.iter().find(|a| a.name == name)
    }
}

/// An IPP request or response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Major and minor version: `(2, 0)` for what we send.
    pub version: (u8, u8),
    /// The operation id of a request, the status code of a response.
    pub code: u16,
    pub request_id: u32,
    pub groups: Vec<Group>,
}

impl Message {
    /// A message with no groups.
    pub fn new(code: u16, request_id: u32) -> Message {
        Message {
            version: (2, 0),
            code,
            request_id,
            groups: Vec::new(),
        }
    }

    /// The first group with delimiter `tag`.
    pub fn group(&self, tag: u8) -> Option<&Group> {
        self.groups.iter().find(|g| g.tag == tag)
    }

    /// The attribute `name` in the first group with delimiter `tag`.
    pub fn get(&self, tag: u8, name: &str) -> Option<&Attribute> {
        self.group(tag)?.get(name)
    }

    /// The first value of attribute `name` in group `tag`, as text.
    pub fn text(&self, tag: u8, name: &str) -> Option<&str> {
        self.get(tag, name)?.first()?.as_str()
    }

    /// The first value of attribute `name` in group `tag`, as a number.
    pub fn int(&self, tag: u8, name: &str) -> Option<i32> {
        self.get(tag, name)?.first()?.as_int()
    }

    /// Whether a response's status is one of the `successful-ok` codes
    /// (`0x0000..=0x00FF`).
    pub fn is_success(&self) -> bool {
        self.code <= 0x00FF
    }
}

/// `job-state` values (RFC 8011 5.3.7).
pub mod job_state {
    pub const PENDING: i32 = 3;
    pub const PENDING_HELD: i32 = 4;
    pub const PROCESSING: i32 = 5;
    pub const PROCESSING_STOPPED: i32 = 6;
    pub const CANCELED: i32 = 7;
    pub const ABORTED: i32 = 8;
    pub const COMPLETED: i32 = 9;

    /// Whether a job in `state` is over: completed, aborted or canceled.
    pub fn is_final(state: i32) -> bool {
        matches!(state, CANCELED | ABORTED | COMPLETED)
    }

    /// The state's keyword-like name, for a status line.
    pub fn name(state: i32) -> &'static str {
        match state {
            PENDING => "pending",
            PENDING_HELD => "held",
            PROCESSING => "printing",
            PROCESSING_STOPPED => "stopped",
            CANCELED => "canceled",
            ABORTED => "aborted",
            COMPLETED => "completed",
            _ => "unknown",
        }
    }
}

/// Operation ids (RFC 8011 5.4.15).
pub mod op {
    pub const PRINT_JOB: u16 = 0x0002;
    pub const VALIDATE_JOB: u16 = 0x0004;
    pub const CANCEL_JOB: u16 = 0x0008;
    pub const GET_JOB_ATTRIBUTES: u16 = 0x0009;
    pub const GET_PRINTER_ATTRIBUTES: u16 = 0x000B;
}

/// A status code's RFC 8011 name, for an error message.
pub fn status_name(code: u16) -> &'static str {
    match code {
        0x0000 => "successful-ok",
        0x0001 => "successful-ok-ignored-or-substituted-attributes",
        0x0002 => "successful-ok-conflicting-attributes",
        0x0400 => "client-error-bad-request",
        0x0401 => "client-error-forbidden",
        0x0402 => "client-error-not-authenticated",
        0x0403 => "client-error-not-authorized",
        0x0404 => "client-error-not-possible",
        0x0405 => "client-error-timeout",
        0x0406 => "client-error-not-found",
        0x0407 => "client-error-gone",
        0x0408 => "client-error-request-entity-too-large",
        0x040A => "client-error-document-format-not-supported",
        0x040B => "client-error-attributes-or-values-not-supported",
        0x040D => "client-error-conflicting-attributes",
        0x0500 => "server-error-internal-error",
        0x0501 => "server-error-operation-not-supported",
        0x0502 => "server-error-service-unavailable",
        0x0503 => "server-error-version-not-supported",
        0x0504 => "server-error-device-error",
        0x0505 => "server-error-temporary-error",
        0x0506 => "server-error-not-accepting-jobs",
        0x0507 => "server-error-busy",
        0x0508 => "server-error-job-canceled",
        _ if code <= 0x00FF => "successful-ok",
        _ if (0x0400..0x0500).contains(&code) => "client-error",
        _ if (0x0500..0x0600).contains(&code) => "server-error",
        _ => "unknown-status",
    }
}
