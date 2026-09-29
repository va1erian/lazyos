//! Wire shapes shared by the S2 system services (`init`, `healthd`, `logd`,
//! `sysmond`) and by `messengerctl`. See the module doc on
//! [`crate::messenger::services`] for the topic-name conventions.
//!
//! Split into [`init`] (the supervisor's `Services`/`Launch`/`ListApps`) and
//! [`health`] (`healthd`, `sysmond`, and `logd`); both are re-exported here so
//! callers keep using `messenger::services::*` unchanged.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Header, Parcel, VERSION};

mod health;
mod init;

pub use health::*;
pub use init::*;

/// The `init` supervisor's registered name.
pub const INIT_NAME: &str = "os.lazy.init";
/// The health aggregator's registered name.
pub const HEALTHD_NAME: &str = "os.lazy.healthd";
/// The structured event log's registered name.
pub const LOGD_NAME: &str = "os.lazy.logd";
/// The system monitor's registered name (issue #144).
pub const SYSMOND_NAME: &str = "os.lazy.sysmond";

/// `os.lazy.init.v1` (interim eight-byte ABI id, see [`super::topics`]).
pub const INIT_INTERFACE: u64 = u64::from_le_bytes(*b"os.init.");
/// `os.lazy.healthd.v1` (interim eight-byte ABI id).
pub const HEALTHD_INTERFACE: u64 = u64::from_le_bytes(*b"os.healt");
/// `os.lazy.logd.v1` (interim eight-byte ABI id).
pub const LOGD_INTERFACE: u64 = u64::from_le_bytes(*b"os.logd.");
/// `os.lazy.system.v1` (interim eight-byte ABI id).
pub const SYSMOND_INTERFACE: u64 = u64::from_le_bytes(*b"os.sysmo");

/// `init` methods.
pub mod init_method {
    /// Snapshot the supervision table.
    pub const SERVICES: u32 = 1;
    /// Launch an app as a session child (issue #158).
    pub const LAUNCH: u32 = 2;
    /// Enumerate the built-in app registry (issue #158).
    pub const LIST_APPS: u32 = 3;
}

/// `healthd` methods.
pub mod healthd_method {
    /// Publish `health/<name>` with status/detail.
    pub const REPORT: u32 = 1;
    /// Snapshot retained health rows plus the summary.
    pub const STATUS: u32 = 2;
}

/// `logd` methods.
pub mod logd_method {
    /// Return the newest `COUNT` records.
    pub const TAIL: u32 = 1;
    /// Return the number of records in the ring.
    pub const COUNT: u32 = 2;
    /// Recompute the hash chain and report `OK`/first bad `INDEX`.
    pub const VERIFY: u32 = 3;
}

/// `sysmond` methods (issue #144).
pub mod sysmond_method {
    /// Return one live system-stats snapshot.
    pub const SNAPSHOT: u32 = 1;
}

/// Shared TLV field ids.
pub mod field {
    /// Service name.
    pub const NAME: u16 = 1;
    /// Service phase (`pending`/`running`/`restarting`/`stopped`/`failed`).
    pub const STATE: u16 = 2;
    /// Task slot the supervisor started, or 0.
    pub const PID: u16 = 3;
    /// Restart count.
    pub const RESTARTS: u16 = 4;
    /// Comma-separated dependency names.
    pub const DEPS: u16 = 5;
    /// One record (service status or health row).
    pub const SERVICE: u16 = 6;
    /// Last known health string for a service.
    pub const HEALTH: u16 = 7;
    /// Health status (`ok`/`degraded`/`down`).
    pub const STATUS: u16 = 8;
    /// Human-readable detail.
    pub const DETAIL: u16 = 9;
    /// Tick the row/record was produced.
    pub const TICK: u16 = 10;
    /// One log record.
    pub const RECORD: u16 = 11;
    /// Log sequence number.
    pub const SEQ: u16 = 12;
    /// Log topic.
    pub const TOPIC: u16 = 13;
    /// Log chain hash.
    pub const HASH: u16 = 14;
    /// Requested/returned count.
    pub const COUNT: u16 = 15;
    /// Verify verdict (1 = chain intact).
    pub const OK: u16 = 16;
    /// First mismatching log index on a broken chain.
    pub const INDEX: u16 = 17;
    /// Aggregate health row.
    pub const SUMMARY: u16 = 18;
    /// Fixed-layout `sysinfo` snapshot bytes (issue #144).
    pub const SYSDATA: u16 = 20;
    /// App id (`LIST_APPS` row, `LAUNCH` request).
    pub const APP: u16 = 21;
    /// Display name (`LIST_APPS` row).
    pub const APP_NAME: u16 = 22;
    /// ELF path resolved from the app id (`LIST_APPS` row).
    pub const APP_PATH: u16 = 23;
    /// Default restart policy (`always`/`on-failure`/`once`).
    pub const APP_RESTART: u16 = 24;
    /// One MIME verb the app handles (repeated).
    pub const APP_VERBS: u16 = 25;
    /// Launch argument string (`LAUNCH` request).
    pub const ARGS: u16 = 26;
    /// Session to launch into (`LAUNCH`; 0 = the caller's own).
    pub const SESSION: u16 = 27;
    /// One app record (`LIST_APPS` reply).
    pub const APP_INFO: u16 = 28;
    /// Structured error (issue #158).
    pub const ERROR: u16 = 29;
}

/// A header for a service parcel of `method` on `interface_id`.
fn header(interface_id: u64, method: u32) -> Header {
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

/// One row of `init`'s supervision table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceStatus {
    pub name: String,
    pub state: String,
    pub pid: u64,
    pub restarts: u64,
    pub deps: String,
    pub health: String,
}

/// One row of `init`'s built-in app registry (issue #158): the S5 start
/// menu's enumeration unit and the resolution table `LAUNCH` uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppInfo {
    /// App id: the lowercase program stem (`top` -> `TOP.ELF`).
    pub id: String,
    /// Display name for menus.
    pub name: String,
    /// On-disk ELF path.
    pub path: String,
    /// Default restart policy (`always`/`on-failure`/`once`).
    pub restart: String,
    /// MIME verbs the app handles (`open`, `edit`, `reveal`).
    pub verbs: Vec<String>,
}

/// The outcome of `init`'s `Launch`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchResult {
    /// App id that was launched.
    pub app: String,
    /// Task slot of the spawned child.
    pub pid: u64,
    /// Session the child was stamped with.
    pub session: u64,
}

/// A decoded `init` `Launch` request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchRequest {
    /// App id from the registry.
    pub app: String,
    /// Argument string passed to the app (may be empty).
    pub args: String,
    /// Target session; `0` means the caller's own session.
    pub session: u64,
}

/// One retained health row (`healthd`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HealthRecord {
    pub name: String,
    pub status: String,
    pub detail: String,
    pub tick: u64,
}

/// One `logd` record (the hash chains over the previous record's hash).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogRecord {
    pub seq: u64,
    pub tick: u64,
    pub topic: String,
    pub detail: String,
    pub hash: u64,
}

/// Iterate `SERVICE`/`RECORD` struct fields of a reply body. Shared by
/// [`init::decode_apps`] and [`health::decode_log_records`]/
/// [`health::fetch_services_with`].
fn for_each_record(
    parcel: &Parcel,
    mut body: impl FnMut(libmessenger::Decoder<'_>) -> super::Result<()>,
) -> super::Result<()> {
    use libmessenger::{Decoder, Kind};
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(super::Error::Parcel)? {
        if field.kind != Kind::Struct {
            continue;
        }
        body(field.nested(0).map_err(super::Error::Parcel)?)?;
    }
    Ok(())
}

/// The first top-level `u64` field (regardless of id).
fn first_u64(parcel: &Parcel) -> Option<u64> {
    all_u64(parcel).next()
}

/// Every top-level `u64` field.
fn all_u64(parcel: &Parcel) -> impl Iterator<Item = u64> + '_ {
    use libmessenger::{Decoder, Kind};
    let mut decoder = Decoder::new(&parcel.body);
    core::iter::from_fn(move || {
        while let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::U64 {
                if let Ok(value) = field.as_u64() {
                    return Some(value);
                }
            }
        }
        None
    })
}
