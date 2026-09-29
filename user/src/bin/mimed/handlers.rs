//! `mimed`'s request dispatch and the open/launch path: resolve a path and
//! verb, optionally ask `init` to launch the app, and publish the
//! fire-and-forget open event on the central broker.
//!
//! Split out of `mimed.rs`, which is past the file-size budget.

use alloc::format;
use alloc::string::ToString;
use user::central;
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
    match message.method() {
        mime::method::GUESS => {
            let path = mime::string_field(&message.parcel, mime::field::PATH)?;
            mime::guess_reply(&db.guess(&path))
        }
        mime::method::LOOKUP => {
            let mime_type = mime::string_field(&message.parcel, mime::field::MIME)?;
            let verb = mime::string_field(&message.parcel, mime::field::VERB)?;
            mime::lookup_reply(apps.lookup(&mime_type, &verb))
        }
        mime::method::VERBS => {
            let mime_type = mime::string_field(&message.parcel, mime::field::MIME)?;
            mime::verbs_reply(&apps.verbs(&mime_type))
        }
        mime::method::OPEN => {
            let path = mime::string_field(&message.parcel, mime::field::PATH)?;
            let verb = mime::string_field(&message.parcel, mime::field::VERB)?;
            let session = caller_session(message);
            let result = open_path(db, apps, bus, &path, &verb, session)?;
            mime::open_reply(&result)
        }
        mime::method::REGISTER => {
            let mime_type = mime::string_field(&message.parcel, mime::field::MIME)?;
            let app = mime::string_field(&message.parcel, mime::field::APP)?;
            let verb = mime::string_field(&message.parcel, mime::field::VERB)?;
            if !valid_mime(&mime_type) || !valid_app_id(&app) || !valid_token(&verb) {
                return Err(Error::Errno(-errno::EINVAL));
            }
            apps.register(&mime_type, &app, &verb);
            Ok(mime::ok_reply(mime::method::REGISTER))
        }
        _ => Err(Error::Errno(-errno::EINVAL)),
    }
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
    let topic = format!("system/events/open/{app}");
    let payload = format!("path={path} mime={mime_type} verb={verb}");
    let published = publish_event(bus, &topic, &payload);
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
/// unreachable and reconnect when a cached connection goes stale.
fn publish_event(bus: &mut Option<central::Bus>, topic: &str, payload: &str) -> bool {
    const ATTEMPTS: usize = 32;
    for _ in 0..ATTEMPTS {
        if bus.is_none() {
            *bus = central::Bus::connect().ok();
        }
        let Some(active) = bus else {
            park_tick();
            continue;
        };
        match active.publish(topic, payload.as_bytes(), false) {
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
