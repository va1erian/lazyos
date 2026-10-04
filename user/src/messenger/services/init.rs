//! `init`: the supervisor's `Services` snapshot, `Launch`/`ListApps`
//! (issue #158) and the orderly `Shutdown` (docs/shutdown.md).
//!
//! The wire shapes are the `midlc`-generated `os.lazy.init.v1` stubs
//! (`idl/init.midl`). Only the structured error field is hand-written.

use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Kind, Parcel};

use crate::messenger::{Endpoint, Error, Result};

use super::{header, resolve_service, ERROR_FIELD, INIT_NAME};

/// The generated `os.lazy.init.v1` stubs (`idl/init.midl`).
pub use messenger_generated::os_lazy_init_v1 as wire;

/// The interface id every `init` parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;

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
    let Ok(endpoint) = resolve_service(INIT_NAME) else {
        return;
    };
    let parcel = Parcel {
        header: header(INTERFACE, wire::METHOD_READY),
        ..Parcel::default()
    };
    let _ = endpoint.send(&parcel);
    // A resolved name aliases `init`'s own endpoint: release this handle,
    // never close the side everyone else calls.
    let _ = endpoint.release();
}

/// `init`'s `Services` request.
pub fn services_request() -> Parcel {
    Parcel {
        header: header(INTERFACE, wire::METHOD_SERVICES),
        ..Parcel::default()
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

/// Decode an `init` `Services` reply.
pub fn decode_services(parcel: &Parcel) -> Result<Vec<ServiceStatus>> {
    let reply = wire::decode_services_reply(&parcel.body).map_err(Error::Parcel)?;
    Ok(reply.services)
}

/// Call `init`'s `Services`.
///
/// Allocates the reply buffer per call; a polling loop should use
/// [`fetch_services_with`] and reuse one buffer.
pub fn fetch_services(endpoint: &Endpoint) -> Result<Vec<ServiceStatus>> {
    let mut buf = alloc::vec![0u8; crate::messenger::DEFAULT_BUFFER];
    fetch_services_with(endpoint, &mut buf)
}

/// [`fetch_services`] with a caller-owned reply buffer.
pub fn fetch_services_with(endpoint: &Endpoint, buf: &mut [u8]) -> Result<Vec<ServiceStatus>> {
    let reply = endpoint.call_with(&services_request(), buf, None)?;
    if let Some(code) = error_field(&reply)? {
        return Err(Error::Init(code));
    }
    decode_services(&reply)
}

/// `init`'s `ListApps` request (issue #158).
pub fn list_apps_request() -> Parcel {
    Parcel {
        header: header(INTERFACE, wire::METHOD_LISTAPPS),
        ..Parcel::default()
    }
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

/// `init`'s `Launch` request: `(app_id, args, session)`. `session` 0 means
/// the caller's own session; only the session's owner (or root) may launch
/// into it.
pub fn launch_request(app: &str, args: &str, session: u64) -> Result<Parcel> {
    let body = wire::encode_launch_args(&wire::LaunchArgs {
        app: alloc::string::String::from(app),
        args: alloc::string::String::from(args),
        session,
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_LAUNCH),
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

/// Decode a `ListApps` reply.
pub fn decode_apps(parcel: &Parcel) -> Result<Vec<AppInfo>> {
    let reply = wire::decode_list_apps_reply(&parcel.body).map_err(Error::Parcel)?;
    Ok(reply.apps)
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

/// Decode a `Launch` reply.
///
/// As in [`decode_launch_request`], an empty `app` is rejected explicitly
/// rather than accepted as the generated default.
pub fn decode_launch(parcel: &Parcel) -> Result<LaunchResult> {
    let result = wire::decode_launch_reply(&parcel.body).map_err(Error::Parcel)?;
    if result.app.is_empty() {
        return Err(Error::Errno(-crate::messenger::errno::EINVAL));
    }
    Ok(result)
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

/// Call `init`'s `ListApps`.
///
/// Allocates the reply buffer per call; a polling loop should use
/// [`fetch_apps_with`] and reuse one buffer.
pub fn fetch_apps(endpoint: &Endpoint) -> Result<Vec<AppInfo>> {
    let mut buf = alloc::vec![0u8; crate::messenger::DEFAULT_BUFFER];
    fetch_apps_with(endpoint, &mut buf)
}

/// [`fetch_apps`] with a caller-owned reply buffer.
pub fn fetch_apps_with(endpoint: &Endpoint, buf: &mut [u8]) -> Result<Vec<AppInfo>> {
    let reply = endpoint.call_with(&list_apps_request(), buf, None)?;
    if let Some(code) = error_field(&reply)? {
        return Err(Error::Init(code));
    }
    decode_apps(&reply)
}

/// Call `init`'s `Launch` and fail on a supervisor error.
pub fn launch(endpoint: &Endpoint, app: &str, args: &str, session: u64) -> Result<LaunchResult> {
    launch_by(endpoint, app, args, session, None)
}

/// [`launch`] with an absolute clock `deadline` on the call, for callers (the
/// compositor) that must not block indefinitely on the supervisor.
pub fn launch_by(
    endpoint: &Endpoint,
    app: &str,
    args: &str,
    session: u64,
    deadline: Option<u64>,
) -> Result<LaunchResult> {
    let reply = endpoint.call(&launch_request(app, args, session)?, deadline)?;
    if let Some(code) = error_field(&reply)? {
        return Err(Error::Init(code));
    }
    decode_launch(&reply)
}

/// `init`'s `Stop` request: stop every running instance of `app`.
pub fn stop_request(app: &str) -> Result<Parcel> {
    let body = wire::encode_stop_args(&wire::StopArgs {
        app: alloc::string::String::from(app),
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_STOP),
        body,
        ..Parcel::default()
    })
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

/// Call `init`'s `Stop`: how many running instances of `app` it ended.
pub fn stop(endpoint: &Endpoint, app: &str) -> Result<u64> {
    let reply = endpoint.call(&stop_request(app)?, None)?;
    if let Some(code) = error_field(&reply)? {
        return Err(Error::Init(code));
    }
    Ok(wire::decode_stop_reply(&reply.body)
        .map_err(Error::Parcel)?
        .stopped)
}

/// `init`'s `Shutdown` request (docs/shutdown.md).
pub fn shutdown_request(mode: u32, reason: &str, force: bool) -> Result<Parcel> {
    let body = wire::encode_shutdown_args(&wire::ShutdownArgs {
        mode,
        reason: alloc::string::String::from(reason),
        force,
    })
    .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INTERFACE, wire::METHOD_SHUTDOWN),
        body,
        ..Parcel::default()
    })
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

/// Call `init`'s `Shutdown`: returns the phase the shutdown is in. The reply
/// comes before anything stops, so a caller may print it; `deadline` bounds
/// the wait for callers that must not block on the supervisor.
pub fn shutdown(
    endpoint: &Endpoint,
    mode: u32,
    reason: &str,
    force: bool,
    deadline: Option<u64>,
) -> Result<alloc::string::String> {
    let reply = endpoint.call(&shutdown_request(mode, reason, force)?, deadline)?;
    if let Some(code) = error_field(&reply)? {
        return Err(Error::Init(code));
    }
    let reply = wire::decode_shutdown_reply(&reply.body).map_err(Error::Parcel)?;
    if !reply.accepted {
        return Err(Error::Errno(-crate::messenger::errno::EAGAIN));
    }
    Ok(reply.phase)
}

/// Resolve [`INIT_NAME`] and launch `app` (a convenience for CLI callers;
/// a polling loop should hold its own endpoint).
pub fn launch_app(app: &str, args: &str, session: u64) -> Result<LaunchResult> {
    let endpoint = resolve_service(INIT_NAME)?;
    launch(&endpoint, app, args, session)
}
