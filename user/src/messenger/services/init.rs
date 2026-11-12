//! `init`: the supervisor's `Services` snapshot, `Launch`/`ListApps`
//! (issue #158) and the orderly `Shutdown` (docs/shutdown.md).
//!
//! The wire shapes and the blocking [`Client`] are the `midlc`-generated
//! `os.lazy.init.v1` stubs (`idl/init.midl`); what is hand-written here is the
//! supervisor's own reply framing (stage C4) and the structured error field.

use libmessenger::Parcel;

use crate::messenger::client::Native;
use crate::messenger::{Error, Result};

use super::header;

/// The generated `os.lazy.init.v1` stubs (`idl/init.midl`).
pub use messenger_generated::os_lazy_init_v1 as wire;

/// The interface id every `init` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// A blocking client of `init`: `Client::try_connect(Native)` (one attempt,
/// for a caller with a cheap fallback) or `connect` (waits for it to register).
pub type Client = wire::Client<Native>;

/// Why an `init` call failed (`init` declares no error codes: a refusal is
/// its errno-style code and sentence).
pub type ClientError = wire::ClientError;

/// The generated method ids.
pub use wire::{
    METHOD_LAUNCH, METHOD_LISTAPPS, METHOD_READY, METHOD_SERVICES, METHOD_SHUTDOWN, METHOD_STOP,
};

/// The `Shutdown` modes (`PowerMode`).
pub use wire::{POWER_MODE_POWER_OFF, POWER_MODE_REBOOT};

/// The shutdown progress payload of the retained `system/power/state` topic.
pub use wire::PowerState;

/// One row of `init`'s supervision table (the generated `ServiceStatus`).
pub use wire::ServiceStatus;

/// One row of `init`'s built-in app registry (the generated `AppInfo`).
pub use wire::AppInfo;

/// One service lifecycle event (the generated `ServiceEvent`): the payload of
/// the retained `system/events/service/<name>` topic.
pub use wire::ServiceEvent;

/// A decoded `Launch` request (the generated `LaunchArgs`).
pub use wire::LaunchArgs as LaunchRequest;

/// The outcome of `init`'s `Launch` (the generated `LaunchReply`).
pub use wire::LaunchReply as LaunchResult;

/// Tell `init` this service is serving (`Ready`, docs/performance-plan.md
/// P7.3): one one-way message, sent once its name is registered. The rows
/// that depend on it start then. Best effort: a service `init` did not start
/// is ignored, and one that cannot reach `init` only delays its dependents
/// to the supervisor's fallback deadline.
pub fn notify_ready() {
    if let Ok(client) = Client::try_connect(Native) {
        // Best effort: a failed send only delays the dependents.
        let _ = client.ready();
    }
}

/// The service name of a concrete `system/events/service/<name>` topic, or
/// `None` for anything else. The broker owns the topic, so a consumer keys on
/// this rather than on a payload field a publisher could spoof.
pub fn service_event_name(topic: &str) -> Option<&str> {
    if !messenger_generated::topics::matches(wire::TOPIC_SYSTEM_EVENTS_SERVICE, topic) {
        return None;
    }
    topic.rsplit('/').next()
}

/// Encode `init`'s `Stop` reply.
pub fn stop_reply(stopped: u64) -> Result<Parcel> {
    let body = wire::encode_stop_reply(&wire::StopReply { stopped }).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_STOP),
        body,
        ..Parcel::default()
    })
}
