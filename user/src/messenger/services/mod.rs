//! Wire shapes shared by the S2 system services (`init`, `healthd`, `logd`,
//! `sysmond`) and by `messengerctl`.
//!
//! Each service's request/reply interface is generated from its `.midl` file
//! (`idl/init.midl`, `idl/healthd.midl`, `idl/logd.midl`, `idl/sysmond.midl`)
//! and re-exported as `wire` from its submodule. The servers are the
//! generated `Server` traits (stage C4).

use libmessenger::{Header, VERSION};

use super::{registry, Endpoint, Result};

pub mod drivers;
pub mod health;
pub mod init;
pub mod lifecycle;
pub mod logd;
pub mod sysmond;

pub use health::HealthRecord;
pub use init::{
    service_event_name, stop_reply, AppInfo, LaunchRequest, LaunchResult, PowerState, ServiceEvent,
    ServiceStatus, POWER_MODE_POWER_OFF, POWER_MODE_REBOOT,
};
pub use logd::LogRecord;

/// The `init` supervisor's registered name.
pub const INIT_NAME: &str = "os.lazy.init";
/// The health aggregator's registered name.
pub const HEALTHD_NAME: &str = "os.lazy.healthd";
/// The structured event log's registered name.
pub const LOGD_NAME: &str = "os.lazy.logd";
/// The system monitor's registered name (issue #144).
pub const SYSMOND_NAME: &str = "os.lazy.sysmond";

/// `os.lazy.init.v1`'s interface id (generated from `idl/init.midl`).
pub const INIT_INTERFACE: u64 = init::INTERFACE;
/// `os.lazy.healthd.v1`'s interface id (generated from `idl/healthd.midl`).
pub const HEALTHD_INTERFACE: u64 = health::INTERFACE;
/// `os.lazy.logd.v1`'s interface id (generated from `idl/logd.midl`).
pub const LOGD_INTERFACE: u64 = logd::INTERFACE;
/// `os.lazy.sysmond.v1`'s interface id (generated from `idl/sysmond.midl`).
pub const SYSMOND_INTERFACE: u64 = sysmond::INTERFACE;

/// A header for a service parcel of `method` on `interface_id`.
pub fn header(interface_id: u64, method: u32) -> Header {
    Header {
        version: VERSION,
        flags: 0,
        interface_id,
        method,
        txn_id: 0,
        reply_to: 0,
        deadline_ns: 0,
    }
}

/// Resolve a service's registered name.
pub fn resolve_service(name: &str) -> Result<Endpoint> {
    registry::resolve(name)
}
