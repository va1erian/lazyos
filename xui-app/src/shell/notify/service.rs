//! `os.lazy.notify` (`os.lazy.notify.v1`, idl/notify.midl): decode,
//! authorize, identify the caller, apply to the center.
//!
//! The caller must be in the shell's login session and be uid 0 or the
//! shell's user, all kernel-stamped (`lazyshell::tray::policy`). Who it is
//! comes from the kernel too: the app `init` launched as the sending task
//! (`init.Services`), or [`lazyshell::notify::policy::Owner::Script`] for an
//! unlabelled program in the session. Nothing in the request names the
//! sender, so an app cannot post as another app or as the system. A refused
//! call prints `SHELL:NOTIFY:DENY` and closes any channel it carried.

use std::rc::Rc;

use lazyshell::notify::policy::{identify, Sender};
use lazyshell::notify::{Refused, Request};
use lazyshell::tray::policy::{Caller, Deny, Shell};
use libmessenger::Parcel;
use messenger_generated::os_lazy_notify_v1 as wire;

use super::super::ctx::Ctx;
use super::super::services;
use crate::server::{error_parcel, reply_parcel, Request as Call, Server};
use crate::sys::{self, errno};

/// Most requests answered per heartbeat.
const REQUESTS_PER_TICK: usize = 8;
/// Receive buffer for one request: a notification with a 64x64 image fits
/// well within it.
const REQUEST_BYTES: usize = 64 * 1024;
/// `ENOTSUP`, for a method this version does not have.
const ENOTSUP: i64 = 95;
/// `ENOSPC`: the sender has too many toasts alive.
const ENOSPC: i64 = 28;
/// `ESRCH`: the caller is not an app `init` launched.
const ESRCH: i64 = 3;
/// After `init` did not answer, requests wait this many ticks.
const INIT_BACKOFF_TICKS: u64 = 100;

/// A failed call: positive errno and friendly text.
type Failure = (i64, &'static str);

/// `init`'s table as read once for one heartbeat's requests.
type Rows = Option<Result<Vec<(String, u64)>, i64>>;

/// The service endpoint, once registered.
#[derive(Default)]
pub struct NotifyService {
    server: Option<Server>,
    next_register: u64,
    init_backoff_until: u64,
    /// The receive buffer, allocated on the first request and reused: the
    /// heartbeat would otherwise zero 64 KiB every 30 ms.
    buf: Vec<u8>,
}

impl NotifyService {
    /// Register when due, then answer what is queued.
    pub fn pump(&mut self, ctx: &Rc<Ctx>) {
        if self.server.is_none() && !self.register(ctx) {
            return;
        }
        let Some(server) = &self.server else {
            return;
        };
        if sys::clock_ticks() < self.init_backoff_until {
            return;
        }
        if self.buf.is_empty() {
            self.buf = vec![0u8; REQUEST_BYTES];
        }
        // One `init.Services` read serves every request of this heartbeat.
        let mut rows: Rows = None;
        for _ in 0..REQUESTS_PER_TICK {
            let call = match server.poll(&mut self.buf) {
                Ok(Some(received)) => received,
                Ok(None) => break,
                Err(code) => {
                    ctx.note("notify-serve", || {
                        format!("SHELL:NOTIFY:RECV:FAIL err={}", -code)
                    });
                    break;
                }
            };
            let reply = answer(ctx, &call, &mut rows);
            if let Some(txn) = call.txn {
                let _ = server.reply(txn, &reply);
            }
            if matches!(rows, Some(Err(_))) {
                self.init_backoff_until = sys::clock_ticks().saturating_add(INIT_BACKOFF_TICKS);
                break;
            }
        }
    }

    fn register(&mut self, ctx: &Rc<Ctx>) -> bool {
        let now = sys::clock_ticks();
        if now < self.next_register {
            return false;
        }
        match Server::register(
            "os.lazy.notify",
            &[wire::INTERFACE_ID],
            &[wire::INTERFACE_NAME],
        ) {
            Ok(server) => {
                self.server = Some(server);
                println!("SHELL:NOTIFY:SERVICE:PASS");
                true
            }
            Err(code) => {
                // A previous shell's name may not be reaped yet.
                self.next_register = now.saturating_add(200);
                ctx.note("notify-register", || {
                    format!("SHELL:NOTIFY:SERVICE:RETRY err={}", -code)
                });
                false
            }
        }
    }
}

/// The reply to one request.
fn answer(ctx: &Rc<Ctx>, call: &Call, rows: &mut Rows) -> Parcel {
    let method = call.parcel.header.method;
    let result = if call.parcel.header.interface_id != wire::INTERFACE_ID {
        Err((ENOTSUP, "not os.lazy.notify.v1"))
    } else {
        identify_owner(ctx, call, rows).and_then(|owner| dispatch(ctx, &owner, method, call))
    };
    match result {
        Ok(body) => reply_parcel(wire::INTERFACE_ID, method, body),
        Err((code, text)) => error_parcel(wire::INTERFACE_ID, method, code, text),
    }
}

/// The owner key of the request's sender, after the session and uid checks.
fn identify_owner(ctx: &Ctx, call: &Call, rows: &mut Rows) -> Result<String, Failure> {
    let origin = call.origin;
    let shell = Shell {
        uid: ctx.uid,
        session: ctx.notify.session,
    };
    let deny = |why: Deny| {
        println!(
            "SHELL:NOTIFY:DENY uid={} label={} why={}",
            origin.uid,
            origin.label_id,
            why.as_str()
        );
        match why {
            Deny::NotAnApp => (ESRCH, "not a launched app"),
            _ => (errno::EACCES, "caller not allowed"),
        }
    };
    let rows = rows
        .get_or_insert_with(services::launched)
        .as_ref()
        .map_err(|code| (code.abs(), "init unreachable"))?;
    let known = rows
        .iter()
        .find(|(_, pid)| *pid == call.sender)
        .is_some_and(|(app, _)| ctx.tray.knows(app));
    if !known && rows.iter().any(|(_, pid)| *pid == call.sender) {
        // An app this shell has not seen yet: read the registry once more.
        ctx.tray.refresh_known(ctx);
    }
    let sender = Sender {
        caller: Caller {
            uid: origin.uid,
            session: origin.session,
        },
        label: origin.label_id,
        pid: call.sender,
    };
    let table = rows.iter().map(|(name, pid)| (name.as_str(), *pid));
    identify(sender, shell, table, |app| ctx.tray.knows(app))
        .map(|owner| owner.key())
        .map_err(deny)
}

fn dispatch(ctx: &Ctx, owner: &str, method: u32, call: &Call) -> Result<Vec<u8>, Failure> {
    let bad = (errno::EINVAL, "malformed request");
    let body = &call.parcel.body;
    let state = &ctx.notify;
    let now = sys::clock_ticks();
    match method {
        wire::METHOD_NOTIFY => {
            let args = wire::decode_notify_args(body).map_err(|_| bad)?;
            let request = Request::from_wire(args).map_err(|why| {
                println!("SHELL:NOTIFY:INVALID owner={owner} field={}", why.as_str());
                (errno::EINVAL, "invalid notification")
            })?;
            let (level, title) = (request.note.level, request.note.title.clone());
            let posted = state.center.borrow_mut().post(owner, request, now);
            match posted {
                Ok(id) => {
                    println!(
                        "SHELL:NOTIFY:POST owner={owner} id={id} level={} title={title}",
                        level.as_str()
                    );
                    wire::encode_notify_reply(&wire::NotifyReply { id }).map_err(|_| bad)
                }
                Err(Refused::RateLimited) => {
                    println!("SHELL:NOTIFY:LIMIT owner={owner}");
                    Err((errno::EBUSY, "too many notifications: slow down"))
                }
                Err(Refused::Full) => Err((ENOSPC, "too many notifications alive")),
            }
        }
        wire::METHOD_CLOSE => {
            let args = wire::decode_close_args(body).map_err(|_| bad)?;
            state.center.borrow_mut().close(owner, args.id, now);
            Ok(Vec::new())
        }
        wire::METHOD_LISTEN => {
            // Decoded, so the event channel is ours: kept, or closed below.
            let args = call.decode(wire::decode_listen_args).map_err(|_| bad)?;
            let kept = state.listeners.borrow_mut().keep(owner, args.events);
            if !kept {
                let _ = sys::msg_close(args.events);
                return Err((ENOSPC, "too many listeners"));
            }
            println!("SHELL:NOTIFY:LISTEN owner={owner}");
            Ok(Vec::new())
        }
        _ => Err((ENOTSUP, "unknown method")),
    }
}
