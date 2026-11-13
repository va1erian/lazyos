//! `os.lazy.shell.tray` (`os.lazy.shell.tray.v1`, idl/tray.midl): decode,
//! authorize, identify the caller, apply to the model.
//!
//! The caller must be in the shell's login session and be uid 0 or the
//! shell's user (`lazyshell::tray::policy`), all kernel-stamped. Its item is
//! keyed by the app `init` launched as the sending task (`init.Services`), so
//! nothing in the request can name another app; a labelled app is also held
//! to its manifest by the kernel before the request gets here. A refused call
//! prints `SHELL:TRAY:DENY` and closes any channel it carried.

use crate::trace::{self, Channel};
use std::rc::Rc;

use lazyshell::tray::item::{Item, Patch};
use lazyshell::tray::policy::{self, Caller as Pol, Deny, Shell};
use lazyshell::tray::{Change, Refused};
use libmessenger::server::{Caller, Fault};
use messenger_generated::os_lazy_shell_tray_v1 as wire;

use super::super::ctx::Ctx;
use super::super::services;
use super::liveness;
use crate::server::Server;
use crate::sys::{self, errno};

/// Most requests answered per heartbeat.
const REQUESTS_PER_TICK: usize = 8;
/// Receive buffer for one request: an item with two 64x64 images and a full
/// menu fits well within it.
const REQUEST_BYTES: usize = 128 * 1024;
/// `ENOSPC`: the tray is full.
const ENOSPC: i64 = 28;
/// `ESRCH`: the caller is not an app `init` launched.
const ESRCH: i64 = 3;

/// The service endpoint, once registered.
#[derive(Default)]
pub struct TrayService {
    server: Option<Server>,
    next_register: u64,
    /// After `init` did not answer, requests wait (queued) until then, so a
    /// stalled `init` costs the desktop one short call, not one per request.
    init_backoff_until: u64,
}

/// `init`'s table as read once for one heartbeat's requests.
type Rows = Option<Result<Vec<(String, u64)>, i64>>;

/// Ticks the tray waits after `init` failed to answer `Services`.
const INIT_BACKOFF_TICKS: u64 = 100;

/// A failed call: positive errno and friendly text.
type Failure = (i64, &'static str);

impl TrayService {
    /// Register when due, then answer what is queued. `true` when the tray
    /// changed.
    pub fn pump(&mut self, ctx: &Rc<Ctx>) -> bool {
        if self.server.is_none() && !self.register(ctx) {
            return false;
        }
        let Some(server) = &self.server else {
            return false;
        };
        if sys::clock_ticks() < self.init_backoff_until {
            return false;
        }
        let mut buf = vec![0u8; REQUEST_BYTES];
        let mut changed = false;
        // One `init.Services` read serves every request of this heartbeat:
        // a consistent snapshot, and at most one short call on the UI thread.
        let mut rows: Rows = None;
        for _ in 0..REQUESTS_PER_TICK {
            let request = match server.poll(&mut buf) {
                Ok(Some(received)) => received,
                Ok(None) => break,
                Err(code) => {
                    ctx.note("tray-serve", || {
                        format!("SHELL:TRAY:RECV:FAIL err={}", -code)
                    });
                    break;
                }
            };
            let mut handler = Handler {
                ctx,
                rows: &mut rows,
                changed: false,
            };
            let reply = wire::dispatch(&mut handler, &request);
            changed |= handler.changed;
            if let Some(txn) = request.txn {
                let _ = server.reply(txn, &reply);
            }
            if matches!(rows, Some(Err(_))) {
                self.init_backoff_until = sys::clock_ticks().saturating_add(INIT_BACKOFF_TICKS);
                break;
            }
        }
        changed
    }

    fn register(&mut self, ctx: &Rc<Ctx>) -> bool {
        let now = sys::clock_ticks();
        if now < self.next_register {
            return false;
        }
        match Server::register(
            trayclient::NAME,
            &[wire::INTERFACE_ID],
            &[wire::INTERFACE_NAME],
        ) {
            Ok(server) => {
                self.server = Some(server);
                trace::line!(Channel::Shell, "TRAY:SERVICE:PASS");
                // Now that `Set` is answered, tell clients to set again.
                ctx.tray.generation.borrow_mut().serving(ctx);
                true
            }
            Err(code) => {
                // A previous shell's name may not be reaped yet.
                self.next_register = now.saturating_add(200);
                ctx.note("tray-register", || {
                    format!("SHELL:TRAY:SERVICE:RETRY err={}", -code)
                });
                false
            }
        }
    }
}

/// The shell's tray state, answering `os.lazy.shell.tray.v1` (the generated
/// server); `changed` records whether any request changed the tray. The
/// channel a `Set` carries is kept only by a successful one: a refused
/// request closes it.
struct Handler<'a> {
    ctx: &'a Rc<Ctx>,
    rows: &'a mut Rows,
    changed: bool,
}

fn fault((code, text): Failure) -> Fault {
    Fault::new(code as u32, text)
}

impl Handler<'_> {
    /// The app the call comes from; an item dropped on the way (a dead app's
    /// leftover) is a change too.
    fn app(&mut self, cx: &Caller) -> Result<String, Failure> {
        let app = identify(self.ctx, cx, self.rows)?;
        self.changed |= same_app(self.ctx, &app, cx)?;
        Ok(app)
    }
}

impl wire::Server for Handler<'_> {
    fn set(&mut self, cx: &Caller, item: wire::Item, events: u64) -> Result<(), Fault> {
        // Decoded, so the event channel is ours: kept by a successful set,
        // closed on any refusal.
        let app = match self.app(cx) {
            Ok(app) => app,
            Err(failure) => {
                let _ = sys::msg_close(events);
                return Err(fault(failure));
            }
        };
        let tray = &self.ctx.tray;
        let set = Item::from_wire(item)
            .map_err(|why| invalid(&app, why.as_str()))
            .and_then(|item| {
                tray.model
                    .borrow_mut()
                    .set(&app, item)
                    .map_err(|why| refused(&app, why))
            });
        if let Err(failure) = set {
            let _ = sys::msg_close(events);
            return Err(fault(failure));
        }
        tray.keep_channel(&app, events, cx.label_id);
        let count = tray.model.borrow().len();
        trace::line!(Channel::Shell, "TRAY:SET app={app} n={count}");
        tray.generation.borrow().restored(tray);
        self.changed = true;
        Ok(())
    }

    fn update(
        &mut self,
        cx: &Caller,
        icon: Option<wire::Icon>,
        tooltip: Option<String>,
        status: Option<u32>,
        badge: Option<String>,
        menu: Option<wire::Menu>,
    ) -> Result<(), Fault> {
        let app = self.app(cx).map_err(fault)?;
        let args = wire::UpdateArgs {
            icon,
            tooltip,
            status,
            badge,
            menu,
        };
        let patch = Patch::from_wire(args).map_err(|why| fault(invalid(&app, why.as_str())))?;
        self.ctx
            .tray
            .model
            .borrow_mut()
            .update(&app, patch)
            .map_err(|why| fault(refused(&app, why)))?;
        trace::line!(Channel::Shell, "TRAY:UPDATE app={app}");
        self.changed = true;
        Ok(())
    }

    fn clear(&mut self, cx: &Caller) -> Result<(), Fault> {
        let app = self.app(cx).map_err(fault)?;
        let tray = &self.ctx.tray;
        let change = tray.model.borrow_mut().clear(&app);
        tray.drop_channel(&app);
        trace::line!(Channel::Shell, "TRAY:CLEAR app={app} why=clear");
        self.changed |= change != Change::Unchanged;
        Ok(())
    }
}

/// The app the request comes from, after the session and uid checks.
fn identify(ctx: &Ctx, request: &Caller, rows: &mut Rows) -> Result<String, Failure> {
    let origin = request;
    let caller = Pol {
        uid: origin.uid,
        session: origin.session,
    };
    let shell = Shell {
        uid: ctx.uid,
        session: ctx.tray.generation.borrow().session(),
    };
    let deny = |why: Deny| {
        trace::line!(
            Channel::Shell,
            "TRAY:DENY uid={} label={} why={}",
            origin.uid,
            origin.label_id,
            why.as_str()
        );
        match why {
            Deny::NotAnApp => (ESRCH, "not a launched app"),
            _ => (errno::EACCES, "caller not allowed"),
        }
    };
    policy::check(caller, shell).map_err(deny)?;
    let rows = rows
        .get_or_insert_with(services::launched)
        .as_ref()
        .map_err(|code| (code.abs(), "init unreachable"))?;
    let caller_row = rows.iter().find(|(_, pid)| *pid == request.sender);
    if caller_row.is_some_and(|(app, _)| !ctx.tray.knows(app)) {
        // An app this shell has not seen yet: read the registry once more.
        ctx.tray.refresh_known(ctx);
    }
    let rows = rows.iter().map(|(name, pid)| (name.as_str(), *pid));
    policy::app_of(rows, request.sender, |app| ctx.tray.knows(app))
        .map(str::to_owned)
        .map_err(deny)
}

/// The request must come from the app that set the item: the label the
/// kernel stamped on it when it was queued must be the one the item was set
/// with. The app is found from the sender's task slot, which a task that
/// exits leaves for reuse, so a request still queued when its sender died
/// could otherwise reach the item of an app launched into the same slot. An
/// item whose channel is dead is the old app's leftover: it goes, and the
/// request proceeds (`Ok(true)`: the tray changed). (Unlabelled built-ins
/// all carry label 0: they are the image's own programs, not packages.)
fn same_app(ctx: &Ctx, app: &str, request: &Caller) -> Result<bool, Failure> {
    let label = request.label_id;
    match ctx.tray.pinned_label(app) {
        Some(pinned) if pinned != label => {
            if liveness::alive(ctx, app) {
                trace::line!(
                    Channel::Shell,
                    "TRAY:DENY uid={} label={label} why=label",
                    request.uid
                );
                return Err((errno::EACCES, "not the app that set the item"));
            }
            liveness::gone(ctx, app);
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn invalid(app: &str, what: &str) -> Failure {
    trace::line!(Channel::Shell, "TRAY:INVALID app={app} field={what}");
    (errno::EINVAL, "invalid tray item")
}

fn refused(app: &str, why: Refused) -> Failure {
    match why {
        Refused::Full => (ENOSPC, "the tray is full"),
        Refused::NoItem => (errno::ENOENT, "no item: call Set first"),
        Refused::Key => (errno::EINVAL, "unusable app id"),
        Refused::Invalid(field) => invalid(app, field.as_str()),
    }
}
