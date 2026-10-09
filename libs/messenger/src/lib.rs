//! Messenger parcel codec (issue #65).
//!
//! The wire format is parcel version 2 (`docs/messenger-core-plan.md` 3.1):
//! a fixed header, a self-describing TLV body, then the *object list*, the
//! kernel objects (channel ends and shared buffers) the body's object fields
//! refer to by index. Everything is little-endian.
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

pub mod envelope;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
mod parcel;
#[cfg(test)]
mod tests;
mod tlv;

pub use parcel::{Buffer, Header, Object, ObjectKind, Parcel, ParcelView, OBJECT_ENTRY_SIZE};
pub use tlv::{Decoder, Encoder, Field, Kind};

use core::fmt;

/// Wire version written into new parcels.
pub const VERSION: u16 = 2;
/// Fixed header size in bytes.
pub const HEADER_SIZE: usize = 48;
/// Largest parcel we will encode or accept.
pub const MAX_PARCEL_BYTES: usize = 1 << 20;
/// Largest TLV body.
pub const MAX_BODY_BYTES: usize = 1 << 20;
/// Largest number of objects (channel ends and buffers) per parcel.
pub const MAX_OBJECTS: usize = 8;
/// Largest number of top-level TLV fields.
pub const MAX_FIELDS: usize = 1024;
/// Largest nesting depth for composite TLV values (array/struct/map/option).
pub const MAX_DEPTH: u8 = 16;

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
    /// A size limit (parcel, body, objects, fields, depth) was exceeded.
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
    /// An object-list entry has an unknown kind or a reserved word that is
    /// not zero.
    BadObject,
    /// An object field's index is not its position in the declared order
    /// (repeated, skipped, out of range or of the other kind), or an object
    /// of the list was claimed by no field.
    BadObjectIndex,
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
            Error::BadObject => "object list entry has an unknown kind",
            Error::BadObjectIndex => "object field does not name its declared slot",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}
