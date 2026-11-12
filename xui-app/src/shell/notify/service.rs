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
use lazyshell::tray::policy::{Caller as Pol, Deny, Shell};
use libmessenger::server::{Caller, Fault};
use messenger_generated::os_lazy_notify_v1 as wire;

use super::super::ctx::Ctx;
use super::super::services;
use crate::server::Server;
use crate::sys::{self, errno};

/// Most requests answered per heartbeat.
const REQUESTS_PER_TICK: usize = 8;
/// Receive buffer for one request: a notification with a 64x64 image fits
/// well within it.
const REQUEST_BYTES: usize = 64 * 1024;
/// `ENOSPC`: the sender has too many toasts alive.
const ENOSPC: i64 = 28;
/// `ESRCH`: the caller is not an app `init` launched.
const ESRCH: i64 = 3;
/// After `init` did not answer, requests wait this many ticks.
const INIT_BACKOFF_TICKS: u64 = 100;

/// A failed call: positive errno and friendly text.
type Failure = (i64, &'static str);

/// `init`'s table as read once for one heartbeat's requests.
type Rows = Option<Result<Vec<(String, u64, u32)>, i64>>;

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
            let reply = wire::dispatch(
                &mut Handler {
                    ctx,
                    rows: &mut rows,
                },
                &call,
            );
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

/// The shell's notification state, answering `os.lazy.notify.v1` (the generated
/// server). `rows` is `init`'s table as read once for this heartbeat.
struct Handler<'a> {
    ctx: &'a Rc<Ctx>,
    rows: &'a mut Rows,
}

fn fault((code, text): Failure) -> Fault {
    Fault::new(code as u32, text)
}

impl Handler<'_> {
    /// The owner of the call, after the session and uid checks, and note it
    /// for which app's windows a click on its toast brings forward: the label
    /// and session the kernel stamped, the same for every thread of the app.
    fn owner(&mut self, cx: &Caller) -> Result<String, Failure> {
        let owner = identify_owner(self.ctx, cx, self.rows)?;
        self.ctx
            .notify
            .listeners
            .borrow_mut()
            .note_sender(&owner, cx.label_id, cx.session);
        Ok(owner)
    }
}

impl wire::Server for Handler<'_> {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &mut self,
        cx: &Caller,
        level: u32,
        title: String,
        body: String,
        icon: Option<wire::Icon>,
        actions: Vec<wire::Action>,
        progress: Option<u32>,
        replaces_id: Option<u64>,
        timeout: Option<u32>,
    ) -> Result<u64, Fault> {
        let owner = self.owner(cx).map_err(fault)?;
        let args = wire::NotifyArgs {
            level,
            title,
            body,
            icon,
            actions,
            progress,
            replaces_id,
            timeout,
        };
        let request = Request::from_wire(args).map_err(|why| {
            println!("SHELL:NOTIFY:INVALID owner={owner} field={}", why.as_str());
            fault((errno::EINVAL, "invalid notification"))
        })?;
        let (level, title) = (request.note.level, request.note.title.clone());
        let now = sys::clock_ticks();
        let posted = self
            .ctx
            .notify
            .center
            .borrow_mut()
            .post(&owner, request, now);
        match posted {
            Ok(id) => {
                println!(
                    "SHELL:NOTIFY:POST owner={owner} id={id} level={} title={title}",
                    level.as_str()
                );
                Ok(id)
            }
            Err(Refused::RateLimited) => {
                println!("SHELL:NOTIFY:LIMIT owner={owner}");
                Err(fault((errno::EBUSY, "too many notifications: slow down")))
            }
            Err(Refused::Full) => Err(fault((ENOSPC, "too many notifications alive"))),
        }
    }

    fn close(&mut self, cx: &Caller, id: u64) -> Result<(), Fault> {
        let owner = self.owner(cx).map_err(fault)?;
        let now = sys::clock_ticks();
        self.ctx.notify.center.borrow_mut().close(&owner, id, now);
        Ok(())
    }

    fn listen(&mut self, cx: &Caller, events: u64) -> Result<(), Fault> {
        // Decoded, so the event channel is ours: kept, or closed on a refusal.
        let owner = match self.owner(cx) {
            Ok(owner) => owner,
            Err(failure) => {
                let _ = sys::msg_close(events);
                return Err(fault(failure));
            }
        };
        let kept = self.ctx.notify.listeners.borrow_mut().keep(&owner, events);
        if !kept {
            let _ = sys::msg_close(events);
            return Err(fault((ENOSPC, "too many listeners")));
        }
        println!("SHELL:NOTIFY:LISTEN owner={owner}");
        Ok(())
    }
}

/// The owner key of the request's sender, after the session and uid checks.
fn identify_owner(ctx: &Ctx, call: &Caller, rows: &mut Rows) -> Result<String, Failure> {
    let origin = call;
    let shell = Shell {
        uid: ctx.uid,
        session: ctx.notify.session,
    };
    let deny = |why: Deny| {
        println!(
            "SHELL:NOTIFY:DENY uid={} label={} sender={} why={}",
            origin.uid,
            origin.label_id,
            call.sender,
            why.as_str()
        );
        match why {
            Deny::NotAnApp => (ESRCH, "not a launched app"),
            _ => (errno::EACCES, "caller not allowed"),
        }
    };
    let rows = rows
        .get_or_insert_with(services::launched_labelled)
        .as_ref()
        .map_err(|code| (code.abs(), "init unreachable"))?;
    let known = rows
        .iter()
        .find(|(_, pid, _)| *pid == call.sender)
        .is_some_and(|(app, ..)| ctx.tray.knows(app));
    if !known && rows.iter().any(|(_, pid, _)| *pid == call.sender) {
        // An app this shell has not seen yet: read the registry once more.
        ctx.tray.refresh_known(ctx);
    }
    let sender = Sender {
        caller: Pol {
            uid: origin.uid,
            session: origin.session,
        },
        label: origin.label_id,
        pid: call.sender,
    };
    let table = rows
        .iter()
        .map(|(name, pid, label)| (name.as_str(), *pid, *label));
    identify(sender, shell, table, |app| ctx.tray.knows(app))
        .map(|owner| owner.key())
        .map_err(deny)
}
