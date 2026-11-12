//! `logd`: the structured, hash-chained event log service (issue #93).
//!
//! The wire shapes are the `midlc`-generated `os.lazy.logd.v1` stubs
//! (`idl/logd.midl`), and so is the blocking [`Client`]; the server is the generated
//! `Server` trait.

use crate::messenger::client::Native;

/// The generated `os.lazy.logd.v1` stubs (`idl/logd.midl`).
pub use messenger_generated::os_lazy_logd_v1 as wire;

/// The interface id every `logd` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// A blocking client of `logd`: `Client::try_connect(Native)`.
pub type Client = wire::Client<Native>;

/// The generated method ids.
pub use wire::{METHOD_COUNT, METHOD_SOURCES, METHOD_TAIL, METHOD_TAILFILE, METHOD_VERIFY};

/// One `logd` record (the hash chains over the previous record's hash).
pub use wire::LogRecord;
