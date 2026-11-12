//! `healthd`: retained health rows and the aggregate snapshot (issue #93).
//!
//! The wire shapes are the `midlc`-generated `os.lazy.healthd.v1` stubs
//! (`idl/healthd.midl`), and so is the blocking [`Client`]; the daemon implements
//! `wire::Server` (stage C4).

use crate::messenger::client::Native;
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
