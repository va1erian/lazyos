//! `init`: the supervisor's `Services` snapshot, and `Launch`/`ListApps`
//! (issue #158).

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Kind, Parcel};

use crate::messenger::{errno, Endpoint, Error, Result};

use super::field;
use super::{
    for_each_record, header, init_method, resolve_service, AppInfo, LaunchRequest, LaunchResult,
    ServiceStatus, INIT_INTERFACE, INIT_NAME,
};

/// `init`'s `Services` request.
pub fn services_request() -> Parcel {
    Parcel {
        header: header(INIT_INTERFACE, init_method::SERVICES),
        ..Parcel::default()
    }
}

/// Encode `init`'s `Services` reply: one `SERVICE` record per row.
pub fn services_reply(statuses: &[ServiceStatus]) -> Result<Parcel> {
    let mut body = Encoder::new();
    for status in statuses {
        let mut record = Encoder::new();
        record
            .string(field::NAME, &status.name)
            .map_err(Error::Parcel)?;
        record
            .string(field::STATE, &status.state)
            .map_err(Error::Parcel)?;
        record.u64(field::PID, status.pid).map_err(Error::Parcel)?;
        record
            .u64(field::RESTARTS, status.restarts)
            .map_err(Error::Parcel)?;
        record
            .string(field::DEPS, &status.deps)
            .map_err(Error::Parcel)?;
        record
            .string(field::HEALTH, &status.health)
            .map_err(Error::Parcel)?;
        body.record(field::SERVICE, &record)
            .map_err(Error::Parcel)?;
    }
    Ok(Parcel {
        header: header(INIT_INTERFACE, init_method::SERVICES),
        body: body.finish(),
        ..Parcel::default()
    })
}

/// `init`'s `ListApps` request (issue #158).
pub fn list_apps_request() -> Parcel {
    Parcel {
        header: header(INIT_INTERFACE, init_method::LIST_APPS),
        ..Parcel::default()
    }
}

/// Encode `init`'s `ListApps` reply: one `APP_INFO` record per app.
pub fn list_apps_reply(apps: &[AppInfo]) -> Result<Parcel> {
    let mut body = Encoder::new();
    for app in apps {
        let mut record = Encoder::new();
        record.string(field::APP, &app.id).map_err(Error::Parcel)?;
        record
            .string(field::APP_NAME, &app.name)
            .map_err(Error::Parcel)?;
        record
            .string(field::APP_PATH, &app.path)
            .map_err(Error::Parcel)?;
        record
            .string(field::APP_RESTART, &app.restart)
            .map_err(Error::Parcel)?;
        for verb in &app.verbs {
            record
                .string(field::APP_VERBS, verb)
                .map_err(Error::Parcel)?;
        }
        body.record(field::APP_INFO, &record)
            .map_err(Error::Parcel)?;
    }
    Ok(Parcel {
        header: header(INIT_INTERFACE, init_method::LIST_APPS),
        body: body.finish(),
        ..Parcel::default()
    })
}

/// `init`'s `Launch` request: `(app_id, args, session)`. `session` 0 means
/// the caller's own session; only the session's owner (or root) may launch
/// into it.
pub fn launch_request(app: &str, args: &str, session: u64) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::APP, app).map_err(Error::Parcel)?;
    if !args.is_empty() {
        body.string(field::ARGS, args).map_err(Error::Parcel)?;
    }
    body.u64(field::SESSION, session).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INIT_INTERFACE, init_method::LAUNCH),
        body: body.finish(),
        ..Parcel::default()
    })
}

/// Encode `init`'s `Launch` reply.
pub fn launch_reply(result: &LaunchResult) -> Result<Parcel> {
    let mut body = Encoder::new();
    body.string(field::APP, &result.app)
        .map_err(Error::Parcel)?;
    body.u64(field::PID, result.pid).map_err(Error::Parcel)?;
    body.u64(field::SESSION, result.session)
        .map_err(Error::Parcel)?;
    Ok(Parcel {
        header: header(INIT_INTERFACE, init_method::LAUNCH),
        body: body.finish(),
        ..Parcel::default()
    })
}

/// Decode a `ListApps` reply.
pub fn decode_apps(parcel: &Parcel) -> Result<Vec<AppInfo>> {
    let mut apps = Vec::new();
    for_each_record(parcel, |mut nested| {
        let mut app = AppInfo::default();
        while let Ok(Some(field)) = nested.next() {
            match (field.kind, field.id) {
                (Kind::String, self::field::APP) => {
                    app.id = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::String, self::field::APP_NAME) => {
                    app.name = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::String, self::field::APP_PATH) => {
                    app.path = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::String, self::field::APP_RESTART) => {
                    app.restart = String::from(field.as_str().map_err(Error::Parcel)?)
                }
                (Kind::String, self::field::APP_VERBS) => app
                    .verbs
                    .push(String::from(field.as_str().map_err(Error::Parcel)?)),
                _ => {}
            }
        }
        apps.push(app);
        Ok(())
    })?;
    Ok(apps)
}

/// Decode a `Launch` request.
pub fn decode_launch_request(parcel: &Parcel) -> Result<LaunchRequest> {
    let mut request = LaunchRequest::default();
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        match (field.kind, field.id) {
            (Kind::String, self::field::APP) => {
                request.app = String::from(field.as_str().map_err(Error::Parcel)?)
            }
            (Kind::String, self::field::ARGS) => {
                request.args = String::from(field.as_str().map_err(Error::Parcel)?)
            }
            (Kind::U64, self::field::SESSION) => {
                request.session = field.as_u64().map_err(Error::Parcel)?
            }
            _ => {}
        }
    }
    if request.app.is_empty() {
        return Err(Error::Errno(-errno::EINVAL));
    }
    Ok(request)
}

/// Decode a `Launch` reply.
pub fn decode_launch(parcel: &Parcel) -> Result<LaunchResult> {
    let mut result = LaunchResult::default();
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        match (field.kind, field.id) {
            (Kind::String, self::field::APP) => {
                result.app = String::from(field.as_str().map_err(Error::Parcel)?)
            }
            (Kind::U64, self::field::PID) => result.pid = field.as_u64().map_err(Error::Parcel)?,
            (Kind::U64, self::field::SESSION) => {
                result.session = field.as_u64().map_err(Error::Parcel)?
            }
            _ => {}
        }
    }
    if result.app.is_empty() {
        return Err(Error::Errno(-errno::EINVAL));
    }
    Ok(result)
}

/// `init`'s error answer: errno-style code plus friendly text, the same
/// shape [`super::super::mime::error_reply`] uses. The client turns the code
/// back into [`Error::Init`].
pub fn init_error_reply(method: u32, error: Error) -> Parcel {
    let code = error.errno().map(|code| -code).unwrap_or(errno::EINVAL);
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder here.
    let _ = body.error(field::ERROR, code as u32, error.message());
    Parcel {
        header: header(INIT_INTERFACE, method),
        body: body.finish(),
        ..Parcel::default()
    }
}

/// The first structured error field, when the reply is a service failure.
pub fn error_field(parcel: &Parcel) -> Result<Option<i64>> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        if field.kind == Kind::Error && field.id == self::field::ERROR {
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
    let reply = endpoint.call(&launch_request(app, args, session)?, None)?;
    if let Some(code) = error_field(&reply)? {
        return Err(Error::Init(code));
    }
    decode_launch(&reply)
}

/// Resolve [`INIT_NAME`] and launch `app` (a convenience for CLI callers;
/// a polling loop should hold its own endpoint).
pub fn launch_app(app: &str, args: &str, session: u64) -> Result<LaunchResult> {
    let endpoint = resolve_service(INIT_NAME)?;
    launch(&endpoint, app, args, session)
}
