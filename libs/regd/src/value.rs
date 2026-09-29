//! The one typed value a path can hold.

use alloc::string::String;
use alloc::vec::Vec;

/// One configuration value.
///
/// `Str` and `Bytes` are limited to [`crate::MAX_VALUE_LEN`] bytes when they
/// enter a [`crate::Store`]; [`Value::size_bytes`] is the weight every
/// variant contributes to the [`crate::MAX_STORE_BYTES`] budget.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Value {
    /// A boolean, stored as one byte.
    Bool(bool),
    /// A signed 64-bit integer.
    I64(i64),
    /// An unsigned 64-bit integer.
    U64(u64),
    /// UTF-8 text.
    Str(String),
    /// Opaque bytes (no UTF-8 requirement).
    Bytes(Vec<u8>),
}

impl Value {
    /// The payload size in bytes used by the limits, independent of the
    /// on-disk framing (which adds a few bytes per entry).
    pub fn size_bytes(&self) -> usize {
        match self {
            Value::Bool(_) => 1,
            Value::I64(_) | Value::U64(_) => 8,
            Value::Str(text) => text.len(),
            Value::Bytes(bytes) => bytes.len(),
        }
    }
}
