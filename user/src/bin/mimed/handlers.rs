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

use super::apps::AppRegistry;
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
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
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
    let app = apps
        .lookup(&mime_type, verb)
        .or_else(|| apps.lookup(&mime_type, mime::DEFAULT_VERB))
        .ok_or(Error::Errno(-errno::ENOENT))?
        .to_string();
    // The gated `init` launch (issue #158): best-effort, so an absent
    // supervisor, an app it does not know, or an app whose ELF is not
    // installed all fall back to the publish-only behavior below.
    let launched = session
        .map(|session| launch_via_init(&app, path, session))
        .unwrap_or(false);
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

/// The caller's kernel-stamped session, when the credential block is readable.
/// `mimed` runs as root, so `cred_get` may read the sender; a failure (no
/// signal today) degrades to publish-only.
fn caller_session(message: &Message) -> Option<u64> {
    let mut cred = sys::Cred::default();
    sys::cred_get(Some(message.sender), &mut cred).ok()?;
    Some(cred.session)
}

/// Ask `init` to launch `app` for `path` in the caller's session. `false` on
/// any failure: the caller still gets the `Open` event.
///
/// The kernel refuses a second synchronous call on a channel while another
/// transaction is open (`-EDEADLK`), and `init`'s endpoint is shared by every
/// client, so a boot-time open walk can race another task's query; retry that
/// specific error, mirroring [`publish_event`].
fn launch_via_init(app: &str, path: &str, session: u64) -> bool {
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
                return true;
            }
            Err(Error::Errno(code)) if code == -errno::EDEADLK => park_tick(),
            Err(_) => return false,
        }
    }
    false
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

/// Sleep one PIT tick by parking on a private channel pair with an expired
/// deadline (userspace has no sleep syscall). The pair is closed again so no
/// channel leaks.
fn park_tick() {
    if let Ok((probe, peer)) = messenger::create_pair() {
        let mut scratch = [0u8; 16];
        let _ = probe.recv_into(&mut scratch, Some(messenger::EXPIRED_DEADLINE));
        let _ = probe.close();
        let _ = peer.close();
    }
}
