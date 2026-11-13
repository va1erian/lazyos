//! `mimed`'s request dispatch and the open/launch path: resolve a path and
//! verb, optionally ask `init` to launch the app, and publish the
//! fire-and-forget open event on the central broker.
//!
//! Split out of `mimed.rs`, which is past the file-size budget.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libmessenger::client::{CallError, Deadline, Errno};
use libmessenger::server::{Caller, Fault};
use user::central;
use user::messenger::client::Native;
use user::messenger::mime::wire;
use user::messenger::services::init::Client as InitClient;
use user::messenger::{self, errno, mime, services, Error};
use user::sys;

use super::apps::{choose, AppRegistry};
use super::db::MimeDb;
use super::prefs::Prefs;
use super::validate::{valid_app_id, valid_mime, valid_token};

/// The service's state, answering requests (the generated [`wire::Server`]).
pub(crate) struct Handler<'a> {
    pub(crate) db: &'a MimeDb,
    pub(crate) apps: &'a mut AppRegistry,
    pub(crate) bus: &'a mut Option<central::Bus>,
    pub(crate) prefs: &'a mut Prefs,
}

impl wire::Server for Handler<'_> {
    fn guess(&mut self, _: &Caller, path: String) -> Result<String, Fault> {
        Ok(self.db.guess(&path))
    }

    fn lookup(&mut self, _: &Caller, mime: String, verb: String) -> Result<Option<String>, Fault> {
        Ok(self.apps.lookup(&mime, &verb).map(String::from))
    }

    fn verbs(&mut self, _: &Caller, mime: String) -> Result<Vec<String>, Fault> {
        Ok(self.apps.verbs(&mime))
    }

    fn open(&mut self, cx: &Caller, path: String, verb: String) -> Result<wire::OpenReply, Fault> {
        // The caller's own choice of default app, by the uid the kernel stamped.
        let (session, uid) = (Some(cx.session), Some(cx.uid));
        let (db, apps) = (self.db, &*self.apps);
        Ok(open_path(
            db, apps, self.bus, self.prefs, &path, &verb, session, uid,
        )?)
    }

    fn register(
        &mut self,
        cx: &Caller,
        mime: String,
        app: String,
        verb: String,
        owner: u32,
    ) -> Result<(), Fault> {
        // Only the package manager (a system service) registers a handler, as
        // only it withdraws one: the registry is global, so any session could
        // otherwise rebind a type for everyone and have the next user's
        // `Open` (an administrator's included) launch its chosen app (audit
        // H11). A user's own choice of app is a preference (`prefs`), not a
        // registration.
        require_system(cx)?;
        require_valid(&mime, &app, &verb)?;
        if !self.apps.register_owned(&mime, &app, &verb, owner) {
            return Err(Errno::NOSPC.into());
        }
        Ok(())
    }

    fn unregister(
        &mut self,
        cx: &Caller,
        mime: String,
        app: String,
        verb: String,
    ) -> Result<(), Fault> {
        // Only the package manager (a system service) may withdraw a
        // registration.
        require_system(cx)?;
        require_valid(&mime, &app, &verb)?;
        self.apps.unregister(&mime, &app, &verb);
        Ok(())
    }
}

/// Refuse a caller that is not an unlabelled system service: it must hold
/// `CAP_SETUID` (kernel-stamped). A uid is not enough (issue #623): a root
/// login session holds no capability.
fn require_system(cx: &Caller) -> Result<(), Fault> {
    if cx.caps & user::sys::CAP_SETUID != 0 && cx.label_id == 0 {
        Ok(())
    } else {
        Err(Errno::PERM.into())
    }
}

fn require_valid(mime: &str, app: &str, verb: &str) -> Result<(), Fault> {
    if valid_mime(mime) && valid_app_id(app) && valid_token(verb) {
        Ok(())
    } else {
        Err(Errno::INVAL.into())
    }
}

/// Guess the path, resolve the app (`verb`, then the default verb; the caller's own choice of app
/// for the type first, `prefs`), ask `init` to launch it when the supervisor is reachable, and
/// publish the launch event on the central broker. `uid` is `None` for the service's own
/// self-test, which has no user.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open_path(
    db: &MimeDb,
    apps: &AppRegistry,
    bus: &mut Option<central::Bus>,
    prefs: &mut Prefs,
    path: &str,
    verb: &str,
    session: Option<u64>,
    uid: Option<u32>,
) -> messenger::Result<mime::OpenResult> {
    let mime_type = db.guess(path);
    // The user's choice is read only when it can matter: two apps handle the type.
    let contested = apps.contested(&mime_type, verb, uid)
        || apps.contested(&mime_type, mime::DEFAULT_VERB, uid);
    let preferred = uid
        .filter(|_| contested)
        .and_then(|uid| prefs.default_app(uid, &mime_type));
    let preferred = preferred.as_deref();
    let (primary, fallback) = apps
        .resolve_preferred(&mime_type, verb, preferred, uid)
        .or_else(|| apps.resolve_preferred(&mime_type, mime::DEFAULT_VERB, preferred, uid))
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
    let mut init: Option<InitClient> = None;
    for _ in 0..ATTEMPTS {
        if init.is_none() {
            init = InitClient::try_connect(Native).ok();
        }
        let Some(active) = &init else {
            park_tick();
            continue;
        };
        match active.launch_within(app, path, session, Deadline::Never) {
            Ok(result) => {
                sys::write_str(&format!(
                    "mimed: launched {} pid {} (session {})\n",
                    result.app, result.pid, result.session
                ));
                return Ok(result);
            }
            Err(error) if error.code() == -errno::EDEADLK => park_tick(),
            Err(CallError::Remote { code, .. }) => return Err(Error::Init(i64::from(code))),
            Err(error) => return Err(Error::Errno(error.code())),
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
