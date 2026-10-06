//! `mimed`'s request dispatch and the open/launch path: resolve a path and
//! verb, optionally ask `init` to launch the app, and publish the
//! fire-and-forget open event on the central broker.
//!
//! Split out of `mimed.rs`, which is past the file-size budget.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use user::central;
use user::messenger::mime::wire;
use user::messenger::{self, errno, mime, services, Endpoint, Error, Message, Parcel};
use user::sys;

use super::apps::{choose, AppRegistry};
use super::db::MimeDb;
use super::validate::{valid_app_id, valid_mime, valid_token};

/// Dispatch one inbound message on the MIME interface.
pub(crate) fn dispatch(
    db: &MimeDb,
    apps: &mut AppRegistry,
    bus: &mut Option<central::Bus>,
    message: &Message,
) -> messenger::Result<Parcel> {
    if message.interface_id() != mime::INTERFACE {
        return Err(Error::Errno(-errno::EINVAL));
    }
    let body = &message.parcel.body;
    let parse = Error::Parcel;
    match message.method() {
        wire::METHOD_GUESS => {
            let args = wire::decode_guess_args(body).map_err(parse)?;
            let mime = db.guess(&args.path);
            reply(
                wire::METHOD_GUESS,
                wire::encode_guess_reply(&wire::GuessReply { mime }),
            )
        }
        wire::METHOD_LOOKUP => {
            let args = wire::decode_lookup_args(body).map_err(parse)?;
            let app = apps.lookup(&args.mime, &args.verb).map(String::from);
            reply(
                wire::METHOD_LOOKUP,
                wire::encode_lookup_reply(&wire::LookupReply { app }),
            )
        }
        wire::METHOD_VERBS => {
            let args = wire::decode_verbs_args(body).map_err(parse)?;
            let verbs = apps.verbs(&args.mime);
            reply(
                wire::METHOD_VERBS,
                wire::encode_verbs_reply(&wire::VerbsReply { verbs }),
            )
        }
        wire::METHOD_OPEN => {
            let args = wire::decode_open_args(body).map_err(parse)?;
            let session = caller_session(message);
            let result = open_path(db, apps, bus, &args.path, &args.verb, session)?;
            reply(wire::METHOD_OPEN, wire::encode_open_reply(&result))
        }
        wire::METHOD_REGISTER => {
            let args = wire::decode_register_args(body).map_err(parse)?;
            if !valid_mime(&args.mime) || !valid_app_id(&args.app) || !valid_token(&args.verb) {
                return Err(Error::Errno(-errno::EINVAL));
            }
            apps.register(&args.mime, &args.app, &args.verb);
            Ok(mime::ok_reply(wire::METHOD_REGISTER))
        }
        wire::METHOD_UNREGISTER => {
            let args = wire::decode_unregister_args(body).map_err(parse)?;
            // Only the package manager (root) may withdraw a registration.
            if !caller_is_root(message) {
                return Err(Error::Errno(-errno::EPERM));
            }
            if !valid_mime(&args.mime) || !valid_app_id(&args.app) || !valid_token(&args.verb) {
                return Err(Error::Errno(-errno::EINVAL));
            }
            apps.unregister(&args.mime, &args.app, &args.verb);
            Ok(mime::ok_reply(wire::METHOD_UNREGISTER))
        }
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
}

/// Whether the sender is uid 0 and unlabelled (kernel-stamped).
fn caller_is_root(message: &Message) -> bool {
    let cred = message.caller();
    cred.uid == 0 && cred.label_id == 0
}

/// Frame an encoded reply body as a parcel of `method`.
fn reply(method: u32, body: Result<Vec<u8>, libmessenger::Error>) -> messenger::Result<Parcel> {
    Ok(mime::parcel(method, body.map_err(Error::Parcel)?))
}

/// Guess the path, resolve the app (`verb`, then the default verb), ask `init`
/// to launch it when the supervisor is reachable, and publish the launch event
/// on the central broker.
pub(crate) fn open_path(
    db: &MimeDb,
    apps: &AppRegistry,
    bus: &mut Option<central::Bus>,
    path: &str,
    verb: &str,
    session: Option<u64>,
) -> messenger::Result<mime::OpenResult> {
    let mime_type = db.guess(path);
    let (primary, fallback) = apps
        .resolve(&mime_type, verb)
        .or_else(|| apps.resolve(&mime_type, mime::DEFAULT_VERB))
        .ok_or(Error::Errno(-errno::ENOENT))?;
    // The gated `init` launch (issue #158): best-effort, so an absent
    // supervisor, an app it does not know, or an app whose ELF is not
    // installed all fall back to the publish-only behavior below. When `init`
    // reports the primary's ELF is not shipped (`-ENOENT`), swap in the
    // registration's fallback: `text/markdown` opens in the Editor when the
    // zig-built Docs app is not in the image.
    let mut app = primary;
    let mut launched = false;
    if let Some(session) = session {
        match launch_via_init(app, path, session) {
            Ok(_) => launched = true,
            Err(Error::Init(code)) if code == errno::ENOENT => {
                app = choose(app, fallback, false);
                if app != primary {
                    launched = launch_via_init(app, path, session).is_ok();
                }
            }
            Err(_) => {}
        }
    }
    let app = app.to_string();
    let event = wire::OpenEvent {
        path: String::from(path),
        mime: mime_type.clone(),
        verb: String::from(verb),
    };
    let published = publish_event(bus, &app, &event);
    // The topic comes from the generated builder, never a hand-typed
    // `format!`; an app id the builder refuses also fails to publish, so the
    // empty topic can never be reported as a success.
    let topic = wire::name_system_events_open(&app).unwrap_or_default();
    Ok(mime::OpenResult {
        app,
        mime: mime_type,
        topic,
        published,
        launched,
    })
}

/// The caller's kernel-stamped session.
fn caller_session(message: &Message) -> Option<u64> {
    Some(message.caller().session)
}

/// Ask `init` to launch `app` for `path` in the caller's session.
///
/// The kernel refuses a second synchronous call on a channel while another
/// transaction is open (`-EDEADLK`), and `init`'s endpoint is shared by every
/// client, so a boot-time open walk can race another task's query; retry that
/// specific error, mirroring [`publish_event`]. Any other supervisor error is
/// returned to the caller: `-ENOENT` means the image does not ship the app (the
/// open path then swaps in its fallback), and an absent supervisor becomes
/// `-EAGAIN` after the retries.
fn launch_via_init(
    app: &str,
    path: &str,
    session: u64,
) -> messenger::Result<services::LaunchResult> {
    // The boot pass has several clients queueing on `init` at once
    // (`logd`/`healthd` subscribing, `messengerctl`'s self-tests), so the
    // retry window is generous: ~1.3 s of parked ticks.
    const ATTEMPTS: usize = 128;
    let mut endpoint: Option<Endpoint> = None;
    for _ in 0..ATTEMPTS {
        if endpoint.is_none() {
            endpoint = services::resolve_service(services::INIT_NAME).ok();
        }
        let Some(active) = &endpoint else {
            park_tick();
            continue;
        };
        match services::launch(active, app, path, session) {
            Ok(result) => {
                sys::write_str(&format!(
                    "mimed: launched {} pid {} (session {})\n",
                    result.app, result.pid, result.session
                ));
                return Ok(result);
            }
            Err(Error::Errno(code)) if code == -errno::EDEADLK => park_tick(),
            Err(error) => return Err(error),
        }
    }
    Err(Error::Errno(-errno::EAGAIN))
}

/// Publish a fire-and-forget launch event through the central broker.
///
/// `messengerd` is the supervisor's first service but its topics name can
/// still land a tick after this service starts, so retry while the broker is
/// unreachable and reconnect when a cached connection goes stale. The generated
/// `publish_system_events_open` helper encodes the typed `OpenEvent` and builds
/// the concrete `system/events/open/<app>` topic.
fn publish_event(bus: &mut Option<central::Bus>, app: &str, event: &wire::OpenEvent) -> bool {
    const ATTEMPTS: usize = 32;
    for _ in 0..ATTEMPTS {
        if bus.is_none() {
            *bus = central::Bus::connect().ok();
        }
        let Some(active) = bus.as_mut() else {
            park_tick();
            continue;
        };
        match wire::publish_system_events_open(active, app, event) {
            Ok(_) => return true,
            Err(_) => {
                *bus = None;
                park_tick();
            }
        }
    }
    false
}

/// Nap one PIT tick's worth between retries ([`user::sys::nap`], a real sleep;
/// this used to park on a throwaway channel pair, since userspace had no
/// sleep call).
fn park_tick() {
    user::sys::nap();
}
