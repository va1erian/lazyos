//! `logd`: the structured, hash-chained event log service (issue #93).
//!
//! The wire shapes are the `midlc`-generated `os.lazy.logd.v1` stubs
//! (`idl/logd.midl`), and so is the blocking [`Client`]; the replies are the
//! daemon's (stage C4).

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::Parcel;

use crate::messenger::client::Native;
use crate::messenger::{Error, Result};

use super::header;

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

/// Encode `logd`'s `Tail` reply.
pub fn log_records_reply(records: &[LogRecord]) -> Result<Parcel> {
    let body = wire::encode_tail_reply(&wire::TailReply {
        records: records.to_vec(),
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_TAIL),
        body,
        ..Parcel::default()
    })
}

/// Encode `logd`'s `Count` reply.
pub fn log_count_reply(count: u64) -> Result<Parcel> {
    let body = wire::encode_count_reply(&wire::CountReply { count }).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_COUNT),
        body,
        ..Parcel::default()
    })
}

/// Encode `logd`'s `Verify` reply: `ok` and the first bad `index` (the record
/// count when the chain is intact).
pub fn log_verify_reply(ok: bool, index: u64) -> Result<Parcel> {
    let body =
        wire::encode_verify_reply(&wire::VerifyReply { ok, index }).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_VERIFY),
        body,
        ..Parcel::default()
    })
}

/// Encode `logd`'s `Sources` reply.
pub fn log_sources_reply(sources: Vec<String>) -> Result<Parcel> {
    let body =
        wire::encode_sources_reply(&wire::SourcesReply { sources }).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_SOURCES),
        body,
        ..Parcel::default()
    })
}

/// Encode `logd`'s `TailFile` reply.
pub fn log_tail_file_reply(lines: Vec<String>) -> Result<Parcel> {
    let body =
        wire::encode_tail_file_reply(&wire::TailFileReply { lines }).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_TAILFILE),
        body,
        ..Parcel::default()
    })
}
