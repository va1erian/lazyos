//! `healthd`: retained health rows and the aggregate snapshot (issue #93).
//!
//! The wire shapes are the `midlc`-generated `os.lazy.healthd.v1` stubs
//! (`idl/healthd.midl`). Only the structured error field is hand-written.

use alloc::vec::Vec;

use libmessenger::Parcel;

use crate::messenger::{Endpoint, Error, Result};

use super::header;

/// The generated `os.lazy.healthd.v1` stubs (`idl/healthd.midl`).
pub use messenger_generated::os_lazy_healthd_v1 as wire;

/// The interface id every `healthd` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// The generated method ids.
pub use wire::{METHOD_REPORT, METHOD_STATUS};

/// One retained health row (the generated `HealthRecord`).
pub use wire::HealthRecord;

/// `healthd`'s `Status` request.
pub fn health_request() -> Parcel {
    Parcel {
        header: header(INTERFACE, wire::METHOD_STATUS),
        ..Parcel::default()
    }
}

/// `healthd`'s `Report` request: publish `health/<name>`.
pub fn health_report_request(name: &str, status: &str, detail: &str) -> Result<Parcel> {
    let body = wire::encode_report_args(&wire::ReportArgs {
        name: alloc::string::String::from(name),
        status: alloc::string::String::from(status),
        detail: alloc::string::String::from(detail),
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_REPORT),
        body,
        ..Parcel::default()
    })
}

/// Encode `healthd`'s snapshot reply: the aggregate `summary`, then one row
/// per retained health record. Used for both `Status` and `Report` answers
/// (the two carry the same shape, as before).
pub fn health_reply(summary: &HealthRecord, records: &[HealthRecord]) -> Result<Parcel> {
    let body = wire::encode_status_reply(&wire::StatusReply {
        summary: summary.clone(),
        records: records.to_vec(),
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_STATUS),
        body,
        ..Parcel::default()
    })
}

/// Call `healthd`'s `Status`; returns the summary and the retained rows.
pub fn fetch_health(endpoint: &Endpoint) -> Result<(HealthRecord, Vec<HealthRecord>)> {
    let reply = endpoint.call(&health_request(), None)?;
    let decoded = wire::decode_status_reply(&reply.body).map_err(Error::Parcel)?;
    Ok((decoded.summary, decoded.records))
}
