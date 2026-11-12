//! `init`: the supervisor's `Services` snapshot, `Launch`/`ListApps`
//! (issue #158) and the orderly `Shutdown` (docs/shutdown.md).
//!
//! The wire shapes and the blocking [`Client`] are the `midlc`-generated
//! `os.lazy.init.v1` stubs (`idl/init.midl`); what is hand-written here is the
//! supervisor's own reply framing (stage C4) and the structured error field.

use libmessenger::{Decoder, Encoder, Kind, Parcel};

use crate::messenger::client::Native;
use crate::messenger::{Error, Result};

use super::{header, ERROR_FIELD};

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

/// Encode `init`'s `Services` reply: one row per supervised service.
pub fn services_reply(statuses: &[ServiceStatus]) -> Result<Parcel> {
    let body = wire::encode_services_reply(&wire::ServicesReply {
        services: statuses.to_vec(),
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_SERVICES),
        body,
        ..Parcel::default()
    })
}

/// Encode `init`'s `ListApps` reply: one record per registered app.
pub fn list_apps_reply(apps: &[AppInfo]) -> Result<Parcel> {
    let body = wire::encode_list_apps_reply(&wire::ListAppsReply {
        apps: apps.to_vec(),
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_LISTAPPS),
        body,
        ..Parcel::default()
    })
}

/// Encode `init`'s `Launch` reply.
pub fn launch_reply(result: &LaunchResult) -> Result<Parcel> {
    let body = wire::encode_launch_reply(result).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_LAUNCH),
        body,
        ..Parcel::default()
    })
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

/// Decode a `Launch` request.
///
/// The generated decoder defaults a missing `app` to the empty string, but
/// the supervisor requires an app id, so keep the explicit check the old
/// hand-written decoder made.
pub fn decode_launch_request(parcel: &Parcel) -> Result<LaunchRequest> {
    let request = wire::decode_launch_args(&parcel.body).map_err(Error::Parcel)?;
    if request.app.is_empty() {
        return Err(Error::Errno(-crate::messenger::errno::EINVAL));
    }
    Ok(request)
}

/// `init`'s error answer: errno-style code plus friendly text, the same shape
/// [`super::error_reply`] uses. The client turns the code back into
/// [`Error::Init`].
pub fn init_error_reply(method: u32, error: Error) -> Parcel {
    let code = error
        .errno()
        .map(|code| -code)
        .unwrap_or(crate::messenger::errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(ERROR_FIELD, code as u32, error.message());
    Parcel {
        header: header(INTERFACE, method),
        body: body.finish(),
        ..Parcel::default()
    }
}

/// The first structured error field, when the reply is a service failure.
pub fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Error && field.id == ERROR_FIELD {
            let (code, _message) = field.error_parts().map_err(Error::Parcel)?;
            return Ok(Some(code as i64));
        }
    }
    Ok(None)
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

/// Decode a `Stop` request; an empty app id is rejected like a `Launch`'s.
pub fn decode_stop_request(parcel: &Parcel) -> Result<wire::StopArgs> {
    let request = wire::decode_stop_args(&parcel.body).map_err(Error::Parcel)?;
    if request.app.is_empty() {
        return Err(Error::Errno(-crate::messenger::errno::EINVAL));
    }
    Ok(request)
}

/// Encode `init`'s `Shutdown` reply: whether a shutdown is running and its
/// phase.
pub fn shutdown_reply(accepted: bool, phase: &str) -> Result<Parcel> {
    let body = wire::encode_shutdown_reply(&wire::ShutdownReply {
        accepted,
        phase: alloc::string::String::from(phase),
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_SHUTDOWN),
        body,
        ..Parcel::default()
    })
}
