//! `healthd`: retained health rows and the aggregate snapshot (issue #93).
//!
//! The wire shapes are the `midlc`-generated `os.lazy.healthd.v1` stubs
//! (`idl/healthd.midl`), and so is the blocking [`Client`]; the replies are
//! the daemon's (stage C4).

use libmessenger::Parcel;

use crate::messenger::client::Native;
use crate::messenger::{Error, Result};

use super::header;

/// The generated `os.lazy.healthd.v1` stubs (`idl/healthd.midl`).
pub use messenger_generated::os_lazy_healthd_v1 as wire;

/// The interface id every `healthd` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// A blocking client of `healthd`: `Client::try_connect(Native)`.
pub type Client = wire::Client<Native>;

/// The generated method ids.
pub use wire::{METHOD_REPORT, METHOD_STATUS};

/// One retained health row (the generated `HealthRecord`).
pub use wire::HealthRecord;

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
