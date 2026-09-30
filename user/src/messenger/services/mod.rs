//! Wire shapes shared by the S2 system services (`init`, `healthd`, `logd`,
//! `sysmond`) and by `messengerctl`.
//!
//! Each service's request/reply interface is generated from its `.midl` file
//! (`idl/init.midl`, `idl/healthd.midl`, `idl/logd.midl`, `idl/sysmond.midl`)
//! and re-exported as `wire` from its submodule. Only the structured error
//! field is hand-written: it is a convention every service shares, not part of
//! any interface, and its id lies outside the generated field range.

use libmessenger::{Encoder, Header, Parcel, VERSION};

use super::{errno, registry, Endpoint, Error, Result};

pub mod health;
pub mod init;
pub mod logd;
pub mod sysmond;

pub use health::{fetch_health, health_reply, health_report_request, health_request, HealthRecord};
pub use init::{
    decode_apps, decode_launch, decode_launch_request, error_field, fetch_apps, fetch_apps_until,
    fetch_apps_with, fetch_services, fetch_services_with, init_error_reply, launch, launch_app,
    launch_by, launch_reply, launch_request, list_apps_reply, list_apps_request,
    service_event_name, services_reply, services_request, AppInfo, LaunchRequest, LaunchResult,
    ServiceEvent, ServiceStatus,
};
pub use logd::{
    decode_log_records, fetch_log_count, fetch_log_tail, fetch_log_verify, log_count_reply,
    log_count_request, log_records_reply, log_tail_request, log_verify_reply, log_verify_request,
    LogRecord,
};
pub use sysmond::{fetch_sysinfo, sysinfo_reply, sysinfo_request};

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

/// The structured error field id in a reply body. The generated fields of
/// every reply use small positional ids, so this can never collide with a
/// success payload.
pub(super) const ERROR_FIELD: u16 = 15;

/// A header for a service parcel of `method` on `interface_id`.
pub(super) fn header(interface_id: u64, method: u32) -> Header {
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

/// A service's error answer for a request on `interface_id`/`method`: the
/// errno-style code plus friendly text in a structured [`ERROR_FIELD`].
pub fn error_reply(interface_id: u64, method: u32, error: Error) -> Parcel {
    let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(ERROR_FIELD, code as u32, error.message());
    Parcel {
        header: header(interface_id, method),
        body: body.finish(),
        ..Parcel::default()
    }
}

/// Resolve a service's registered name.
pub fn resolve_service(name: &str) -> Result<Endpoint> {
    registry::resolve(name)
}
