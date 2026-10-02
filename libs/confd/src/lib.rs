//! Configuration store logic for `confd` (issue #259).
//!
//! `docs/confd-plan.md` v1 (§1–§5) needs one typed value per
//! hierarchical path, crash-safe persistence, and uid-checked access. This
//! crate is that logic without Messenger or filesystem dependencies: the
//! `confd` service wraps it, and the host runs the same code under
//! `cargo test -p confd`.
//!
//! # Contract
//!
//! * A path is `sys...` or `user/<uid>...` ([`validate_path`]); at most
//!   [`MAX_PATH_LEN`] bytes, segments `[a-z0-9_.-]+`.
//! * One [`Value`] per path, at most [`MAX_VALUE_LEN`] bytes; the sum over
//!   all entries of `path.len() + value size` is at most [`MAX_STORE_BYTES`].
//! * [`Store::get`]/[`Store::set`]/[`Store::delete`] return a denial for
//!   paths the caller cannot read or write, which is distinct from absence, so
//!   callers cannot probe other users' subtrees. [`Store::list`] instead
//!   filters unreadable paths out of its result.
//! * [`encode`]/[`decode`] use a strict length-prefixed format with a magic
//!   header and a CRC-32 trailer; `decode` re-validates every path and limit
//!   and never panics, over-allocates or accepts trailing bytes.
//! * [`persist`]/[`load`] go through a [`StoreFs`]: `persist` writes
//!   `store.tmp`, fsyncs it, then renames it over `store`, so a crash at any
//!   point leaves the previous or the next complete store, never a torn one.
//!   `load` clears a leftover temporary file and moves a corrupt store aside.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod codec;
pub mod dir;
pub mod fs;
pub mod path;
pub mod service;
pub mod store;
pub mod value;

pub use codec::{decode, encode, DecodeError, MAX_ENCODED_LEN};
pub use fs::{
    load, load_read_only, persist, retire, StoreFs, CORRUPT_FILE, MIGRATED_FILE, STORE_FILE,
    TMP_FILE,
};
pub use path::validate_path;
pub use service::{announceable, ChangeSink, Confd, Migration, ServiceError};
pub use store::{Caller, Change, Error, Store};
pub use value::Value;

/// Largest accepted path, in bytes.
pub const MAX_PATH_LEN: usize = 256;
/// Largest accepted value payload, in bytes.
pub const MAX_VALUE_LEN: usize = 4 * 1024;
/// Largest total `path + value` bytes the store may hold.
pub const MAX_STORE_BYTES: usize = 1024 * 1024;
