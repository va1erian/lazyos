//! `sysmond`: one live system-stats snapshot (issue #144).
//!
//! The wire shape is the `midlc`-generated `os.lazy.sysmond.v1` stub
//! (`idl/sysmond.midl`). The generated server answers it.

/// The generated `os.lazy.sysmond.v1` stubs (`idl/sysmond.midl`).
pub use messenger_generated::os_lazy_sysmond_v1 as wire;

/// The interface id every `sysmond` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// A blocking client of `sysmond`: `Client::try_connect(Native)`.
pub type Client = wire::Client<crate::messenger::client::Native>;

/// The generated `Snapshot` method id.
pub use wire::METHOD_SNAPSHOT;
