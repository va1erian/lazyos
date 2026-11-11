//! `sysmond`: one live system-stats snapshot (issue #144).
//!
//! The wire shape is the `midlc`-generated `os.lazy.sysmond.v1` stub
//! (`idl/sysmond.midl`). Only the structured error field is hand-written.

use libmessenger::Parcel;

use crate::messenger::{errno, Error, Result};

use super::header;

/// The generated `os.lazy.sysmond.v1` stubs (`idl/sysmond.midl`).
pub use messenger_generated::os_lazy_sysmond_v1 as wire;

/// The interface id every `sysmond` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// A blocking client of `sysmond`: `Client::try_connect(Native)`.
pub type Client = wire::Client<crate::messenger::client::Native>;

/// The generated `Snapshot` method id.
pub use wire::METHOD_SNAPSHOT;

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
