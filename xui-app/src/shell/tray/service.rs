//! `os.lazy.shell.tray` (`os.lazy.shell.tray.v1`, idl/tray.midl): decode,
//! authorize, identify the caller, apply to the model.
//!
//! The caller must be in the shell's login session and be uid 0 or the
//! shell's user (`lazyshell::tray::policy`), all kernel-stamped. Its item is
//! keyed by the app `init` launched as the sending task (`init.Services`), so
//! nothing in the request can name another app; a labelled app is also held
//! to its manifest by the kernel before the request gets here. A refused call
//! prints `SHELL:TRAY:DENY` and closes any channel it carried.

use std::rc::Rc;

use lazyshell::tray::item::{Item, Patch};
use lazyshell::tray::policy::{self, Caller, Deny, Shell};
use lazyshell::tray::{Change, Refused};
use libmessenger::Parcel;
use messenger_generated::os_lazy_shell_tray_v1 as wire;

use super::super::ctx::Ctx;
use super::super::services;
use super::liveness;
use crate::server::{error_parcel, reply_parcel, Request, Server};
use crate::sys::{self, errno};

/// Most requests answered per heartbeat.
const REQUESTS_PER_TICK: usize = 8;
/// Receive buffer for one request: an item with two 64x64 images and a full
/// menu fits well within it.
const REQUEST_BYTES: usize = 128 * 1024;
/// `ENOTSUP`, for a method this version does not have.
const ENOTSUP: i64 = 95;
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
            let (request, channel) = match server.poll_keeping_channel(&mut buf) {
                Ok(Some(received)) => received,
                Ok(None) => break,
                Err(code) => {
                    ctx.note("tray-serve", || {
                        format!("SHELL:TRAY:RECV:FAIL err={}", -code)
                    });
                    break;
                }
            };
            let (reply, did) = answer(ctx, &request, channel, &mut rows);
            changed |= did;
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
                println!("SHELL:TRAY:SERVICE:PASS");
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

/// The reply to one request, and whether the tray changed. A transferred
/// channel is kept only by a successful `Set`.
fn answer(
    ctx: &Rc<Ctx>,
    request: &Request,
    channel: Option<u64>,
    rows: &mut Rows,
) -> (Parcel, bool) {
    let method = request.parcel.header.method;
    let mut channel = channel;
    // An item dropped on the way (a dead app's leftover) is a change too.
    let mut dropped = false;
    let result = if request.parcel.header.interface_id != wire::INTERFACE_ID {
        Err((ENOTSUP, "not os.lazy.shell.tray.v1"))
    } else {
        identify(ctx, request, rows)
            .and_then(|app| {
                dropped = same_app(ctx, &app, request)?;
                Ok(app)
            })
            .and_then(|app| {
                let label = request.origin.label_id;
                dispatch(ctx, &app, label, method, &request.parcel.body, &mut channel)
            })
    };
    // A channel no successful `Set` took is not kept.
    if let Some(handle) = channel {
        let _ = sys::msg_close(handle);
    }
    match result {
        Ok(changed) => (
            reply_parcel(wire::INTERFACE_ID, method, Vec::new()),
            changed || dropped,
        ),
        Err((code, text)) => (
            error_parcel(wire::INTERFACE_ID, method, code, text),
            dropped,
        ),
    }
}

/// The app the request comes from, after the session and uid checks.
fn identify(ctx: &Ctx, request: &Request, rows: &mut Rows) -> Result<String, Failure> {
    let origin = request.origin;
    let caller = Caller {
        uid: origin.uid,
        session: origin.session,
    };
    let shell = Shell {
        uid: ctx.uid,
        session: ctx.tray.generation.borrow().session(),
    };
    let deny = |why: Deny| {
        println!(
            "SHELL:TRAY:DENY uid={} label={} why={}",
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
fn same_app(ctx: &Ctx, app: &str, request: &Request) -> Result<bool, Failure> {
    let label = request.origin.label_id;
    match ctx.tray.pinned_label(app) {
        Some(pinned) if pinned != label => {
            if liveness::alive(ctx, app) {
                println!(
                    "SHELL:TRAY:DENY uid={} label={label} why=label",
                    request.origin.uid
                );
                return Err((errno::EACCES, "not the app that set the item"));
            }
            liveness::gone(ctx, app);
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn dispatch(
    ctx: &Ctx,
    app: &str,
    label: u32,
    method: u32,
    body: &[u8],
    channel: &mut Option<u64>,
) -> Result<bool, Failure> {
    let bad = (errno::EINVAL, "malformed request");
    let tray = &ctx.tray;
    match method {
        wire::METHOD_SET => {
            if channel.is_none() {
                return Err((errno::EINVAL, "no event channel"));
            }
            let args = wire::decode_set_args(body).map_err(|_| bad)?;
            let item = Item::from_wire(args.item).map_err(|why| invalid(app, why.as_str()))?;
            tray.model
                .borrow_mut()
                .set(app, item)
                .map_err(|why| refused(app, why))?;
            if let Some(handle) = channel.take() {
                tray.keep_channel(app, handle, label);
            }
            let count = tray.model.borrow().len();
            println!("SHELL:TRAY:SET app={app} n={count}");
            tray.generation.borrow().restored(tray);
            Ok(true)
        }
        wire::METHOD_UPDATE => {
            let args = wire::decode_update_args(body).map_err(|_| bad)?;
            let patch = Patch::from_wire(args).map_err(|why| invalid(app, why.as_str()))?;
            tray.model
                .borrow_mut()
                .update(app, patch)
                .map_err(|why| refused(app, why))?;
            println!("SHELL:TRAY:UPDATE app={app}");
            Ok(true)
        }
        wire::METHOD_CLEAR => {
            let change = tray.model.borrow_mut().clear(app);
            tray.drop_channel(app);
            println!("SHELL:TRAY:CLEAR app={app} why=clear");
            Ok(change != Change::Unchanged)
        }
        _ => Err((ENOTSUP, "unknown method")),
    }
}

fn invalid(app: &str, what: &str) -> Failure {
    println!("SHELL:TRAY:INVALID app={app} field={what}");
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
