//! `sysmond`: one live system-stats snapshot (issue #144).
//!
//! The wire shape is the `midlc`-generated `os.lazy.sysmond.v1` stub
//! (`idl/sysmond.midl`). Only the structured error field is hand-written.

use libmessenger::Parcel;

use crate::messenger::{errno, Endpoint, Error, Result};

use super::header;

/// The generated `os.lazy.sysmond.v1` stubs (`idl/sysmond.midl`).
pub use messenger_generated::os_lazy_sysmond_v1 as wire;

/// The interface id every `sysmond` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// The generated `Snapshot` method id.
pub use wire::METHOD_SNAPSHOT;

/// `sysmond`'s `Snapshot` request (issue #144).
pub fn sysinfo_request() -> Parcel {
    Parcel {
        header: header(INTERFACE, wire::METHOD_SNAPSHOT),
        ..Parcel::default()
    }
}

/// Encode `sysmond`'s `Snapshot` reply: the raw fixed-layout `sysinfo`
/// snapshot bytes.
pub fn sysinfo_reply(snapshot: &crate::sysinfo::Snapshot) -> Result<Parcel> {
    let mut wire_bytes = [0u8; crate::sysinfo::SIZE];
    if !snapshot.write_bytes(&mut wire_bytes) {
        return Err(Error::Errno(-errno::E2BIG));
    }
    let body = wire::encode_snapshot_reply(&wire::SnapshotReply {
        data: wire_bytes.to_vec(),
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_SNAPSHOT),
        body,
        ..Parcel::default()
    })
}

/// Call `sysmond`'s `Snapshot` and decode the fixed-layout reply; a service
/// failure comes back as its original errno.
pub fn fetch_sysinfo(endpoint: &Endpoint) -> Result<crate::sysinfo::Snapshot> {
    let reply = endpoint.call(&sysinfo_request(), None)?;
    if let Some(code) = super::error_field(&reply)? {
        return Err(Error::Errno(-code));
    }
    let decoded = wire::decode_snapshot_reply(&reply.body).map_err(Error::Parcel)?;
    crate::sysinfo::decode_bytes(&decoded.data).ok_or(Error::Errno(-errno::EINVAL))
}
