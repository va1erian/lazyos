//! `logd`: the structured, hash-chained event log service (issue #93).
//!
//! The wire shapes are the `midlc`-generated `os.lazy.logd.v1` stubs
//! (`idl/logd.midl`). Only the structured error field is hand-written.

use alloc::vec::Vec;

use libmessenger::Parcel;

use crate::messenger::{Endpoint, Error, Result};

use super::header;

/// The generated `os.lazy.logd.v1` stubs (`idl/logd.midl`).
pub use messenger_generated::os_lazy_logd_v1 as wire;

/// The interface id every `logd` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// The generated method ids.
pub use wire::{METHOD_COUNT, METHOD_TAIL, METHOD_VERIFY};

/// One `logd` record (the hash chains over the previous record's hash).
pub use wire::LogRecord;

/// `logd`'s `Tail` request. The count is optional on the wire; an absent one
/// means the service's default.
pub fn log_tail_request(count: u64) -> Parcel {
    // A fresh encoder has room for one small option; an encoding error is
    // impossible and would fall back to the default count.
    let body = wire::encode_tail_args(&wire::TailArgs { count: Some(count) }).unwrap_or_default();
    Parcel {
        header: header(INTERFACE, wire::METHOD_TAIL),
        body,
        ..Parcel::default()
    }
}

/// `logd`'s `Count` request.
pub fn log_count_request() -> Parcel {
    Parcel {
        header: header(INTERFACE, wire::METHOD_COUNT),
        ..Parcel::default()
    }
}

/// `logd`'s `Verify` request.
pub fn log_verify_request() -> Parcel {
    Parcel {
        header: header(INTERFACE, wire::METHOD_VERIFY),
        ..Parcel::default()
    }
}

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

/// Decode a `Tail` reply into records.
pub fn decode_log_records(parcel: &Parcel) -> Result<Vec<LogRecord>> {
    let reply = wire::decode_tail_reply(&parcel.body).map_err(Error::Parcel)?;
    Ok(reply.records)
}

/// Call `logd`'s `Tail`.
pub fn fetch_log_tail(endpoint: &Endpoint, count: u64) -> Result<Vec<LogRecord>> {
    let reply = endpoint.call(&log_tail_request(count), None)?;
    decode_log_records(&reply)
}

/// Call `logd`'s `Count`.
pub fn fetch_log_count(endpoint: &Endpoint) -> Result<u64> {
    let reply = endpoint.call(&log_count_request(), None)?;
    let decoded = wire::decode_count_reply(&reply.body).map_err(Error::Parcel)?;
    Ok(decoded.count)
}

/// Call `logd`'s `Verify`; returns `(intact, first bad index)`.
pub fn fetch_log_verify(endpoint: &Endpoint) -> Result<(bool, u64)> {
    let reply = endpoint.call(&log_verify_request(), None)?;
    let decoded = wire::decode_verify_reply(&reply.body).map_err(Error::Parcel)?;
    Ok((decoded.ok, decoded.index))
}
