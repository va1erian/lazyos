//! The service thread: it registers `app.os.lazy.webd.light`, answers requests, pumps the engine's
//! news out to the clients and follows `init`'s lifecycle line, and it is parked the rest of the
//! time.
//!
//! **Wake-up.** One `wait_any` parks the thread on three things at once: the registered endpoint (a
//! request), the lifecycle channel (a `Quit` from `init`) and the [doorbell](crate::doorbell)
//! descriptor (WebKit's thread queued news, or SIGTERM arrived). It also has a deadline of
//! [`IDLE_MS`] so the `Ping`s that find a dead client and the watchdog's probes still run when
//! nothing else happens. A pass is: clear the doorbell, answer every queued request, pump the
//! queue, look at the lifecycle, park. Clearing first means a ring that lands during the pass leaves
//! the descriptor readable and the next park returns at once; nothing is lost and nothing spins.
//! With no view the thread wakes once a second to find nothing to do (docs/webd.md has the measured
//! idle cost).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use messenger_generated::os_lazy_webd_light_v1 as wire;
use xui_app::resident::{Lifecycle, Wake};
use xui_app::server::{Request, Server};
use xui_app::sys;
use xui_app::trace::{self, Budget, Channel};
use xui_webd::events::EventQueue;
use xui_webd::ffi::FfiEngine;
use xui_webd::ident::{ClientKey, Origin};
use xui_webd::limits::REQUEST_BYTES;
use xui_webd::{Call, Service};

use crate::fabric::RealFabric;
use crate::heartbeat::{Beat, Heartbeat};
use crate::lines::{lines_of, Line};
use crate::{doorbell, selftest};

/// The name the light interface is registered under (`docs/webd-plan.md` section 3.1: a labelled app
/// registers `app.<id>.<name>` for an interface of its own domain `<id>.<name>.v<N>`).
pub const NAME: &str = "app.os.lazy.webd.light";

/// How long the service thread parks at most, in milliseconds.
const IDLE_MS: u64 = 1000;
/// Requests answered in one pass: a client flooding oneway input cannot keep the pump, the
/// lifecycle line and the watchdog waiting.
const PER_PASS: usize = 256;

/// Why the loop ended; the process's exit status is derived from it.
pub const EXIT_CLEAN: i32 = 0;
pub const EXIT_NOT_SERVING: i32 = 1;
/// The engine's main thread stopped answering; it cannot be asked to quit.
pub const EXIT_HUNG: i32 = 4;

/// Set by `Quit` from `init` and by SIGTERM: leave the loop.
static QUIT: AtomicBool = AtomicBool::new(false);
/// Set by the probe task on WebKit's main thread.
static ANSWERED: AtomicBool = AtomicBool::new(true);

/// Marker lines that can repeat in a loop (a client that never reads, one that sends bad requests):
/// at most 20 per ten seconds.
static FLOOD: Budget = Budget::new(20, 1000);

/// Ask the loop to end (any thread, and a signal handler: an atomic store and a `write`).
pub fn request_quit() {
    QUIT.store(true, Ordering::Release);
    doorbell::ring();
}

/// The probe the watchdog posts to WebKit's main thread.
extern "C" fn beat(_context: *mut std::ffi::c_void) {
    ANSWERED.store(true, Ordering::Release);
}

fn now_ms() -> u64 {
    sys::monotonic_ns() / 1_000_000
}

/// A client's label as text for the markers; `none` for a caller without one.
fn client_name(client: ClientKey) -> String {
    if client.label == 0 {
        return String::from("none");
    }
    let mut out = [0u8; lazyos_sys::cred::MAX_LABEL_BYTES];
    match lazyos_sys::cred::label_name(client.label, &mut out) {
        Ok(len) => String::from_utf8_lossy(&out[..len]).into_owned(),
        Err(_) => format!("label#{}", client.label),
    }
}

type Svc = Service<FfiEngine, RealFabric>;

/// Print what the service reported since the last look.
fn flush_notes(service: &mut Svc) {
    for note in service.take_notes() {
        for line in lines_of(note, &mut client_name) {
            match line {
                Line::Create { client, view } => {
                    trace::line!(Channel::Webd, "VIEW:CREATE client={client} view={view}")
                }
                Line::Destroy { view } => trace::line!(Channel::Webd, "VIEW:DESTROY view={view}"),
                Line::Lost { view, reason } => {
                    trace::line!(Channel::Webd, "LOST view={view} reason={reason}")
                }
                Line::Note(text) => trace::line!(Channel::Webd, "NOTE:{text}"),
                Line::Flood(text) => {
                    if trace::within(&FLOOD, Channel::Webd) {
                        trace::line!(Channel::Webd, "NOTE:{text}");
                    }
                }
            }
        }
    }
}

/// Answer one request. The reply of a oneway message is dropped.
fn answer(server: &Server, service: &mut Svc, mut request: Request) {
    let origin = Origin {
        uid: request.origin.uid,
        label: request.origin.label_id,
        session: request.origin.session,
        sender: request.sender,
    };
    let call = Call {
        origin,
        parcel: std::mem::take(&mut request.parcel),
        objects: request.take_objects(),
    };
    let reply = service.handle(call);
    if let Some(txn) = request.txn {
        let _ = server.reply(txn, &reply);
    }
}

/// Every request that is queued now, up to [`PER_PASS`]; the number answered.
fn serve_requests(server: &Server, service: &mut Svc, buf: &mut [u8]) -> usize {
    let mut answered = 0;
    while answered < PER_PASS {
        match server.poll(buf) {
            Ok(Some(request)) => {
                answer(server, service, request);
                answered += 1;
            }
            Ok(None) => break,
            Err(code) => {
                trace::fail!(Channel::Webd, "RECV", "err={}", -code);
                break;
            }
        }
    }
    answered
}

/// What `init` and the tray asked since the last look; `true` to quit.
fn lifecycle(life: &mut Option<Lifecycle>) -> bool {
    let Some(life) = life.as_mut() else {
        return false;
    };
    let mut quit = false;
    for wake in life.poll() {
        match wake {
            Wake::Quit(grace) => {
                trace::line!(Channel::Webd, "QUIT:REQUESTED grace_ms={grace}");
                quit = true;
            }
            Wake::Reopen(args) => {
                // webd has no window to show; a second launch with the self-test path runs the
                // built-in client (docs/webd.md).
                if args == selftest::ARG {
                    selftest::start();
                }
            }
            Wake::Tray(_) => {}
        }
    }
    quit
}

/// Park until a request, a lifecycle event, a ring of the doorbell or the idle deadline.
fn park(server: &Server, life: &Option<Lifecycle>) {
    let mut handles = vec![server.endpoint()];
    if let Some(life) = life {
        handles.push(life.channel());
    }
    let flags = doorbell::fd().map_or(0, |fd| sys::WAIT_FD | (fd as u64) << sys::WAIT_FD_SHIFT);
    let deadline = sys::monotonic_ns().saturating_add(IDLE_MS * 1_000_000);
    match sys::msg_wait_any_ns(&handles, flags, deadline) {
        Ok(_) => {}
        // A signal (SIGTERM) interrupts the wait: the loop looks at the quit flag next.
        Err(code) if code == -sys::errno::ETIMEDOUT || code == -sys::errno::EINTR => {}
        Err(code) => {
            // A wait that fails at once must not become a busy loop.
            trace::fail!(Channel::Webd, "WAIT", "err={}", -code);
            sys::sleep_millis(50);
        }
    }
}

/// Serve until `Quit`, SIGTERM or a hang; returns the exit status ([`EXIT_CLEAN`], ...). Runs on the
/// process's first thread, with `queue` the sink the engine's callbacks were installed with.
pub fn run(queue: Arc<EventQueue>) -> i32 {
    let session = sys::cred_get(None).map_or(0, |cred| cred.session);
    trace::pass!(Channel::Webd, "UP");
    let server = match Server::register(NAME, &[wire::INTERFACE_ID], &[wire::INTERFACE_NAME]) {
        Ok(server) => server,
        Err(code) => {
            trace::fail!(Channel::Webd, "SERVE", "register {NAME}: err={}", -code);
            return EXIT_NOT_SERVING;
        }
    };
    trace::line!(Channel::Webd, "SERVE:{NAME}");
    let mut life = match Lifecycle::watch() {
        Ok(life) => Some(life),
        // Started by hand rather than by `init`: SIGTERM still ends it.
        Err(code) => {
            trace::warn!(Channel::Webd, "WATCH", "no lifecycle line: err={}", -code);
            None
        }
    };
    let mut service: Svc = Service::new(FfiEngine, RealFabric, queue, session);
    let mut buf = vec![0u8; REQUEST_BYTES];
    let mut watchdog = Heartbeat::new();
    let mut status = EXIT_CLEAN;
    loop {
        doorbell::clear();
        serve_requests(&server, &mut service, &mut buf);
        service.pump(now_ms());
        flush_notes(&mut service);
        if lifecycle(&mut life) || QUIT.load(Ordering::Acquire) {
            break;
        }
        match watchdog.poll(
            now_ms(),
            ANSWERED.load(Ordering::Acquire),
            service.view_count() > 0,
        ) {
            Beat::Idle => {}
            Beat::Send => {
                ANSWERED.store(false, Ordering::Release);
                // SAFETY: `beat` touches only a static and ignores its context.
                unsafe { FfiEngine::run_on_main(beat, std::ptr::null_mut()) };
            }
            Beat::Hung => {
                trace::fail!(
                    Channel::Webd,
                    "HANG",
                    "WebKit's main thread has not answered"
                );
                service.engine_lost();
                service.pump(now_ms());
                flush_notes(&mut service);
                status = EXIT_HUNG;
                break;
            }
        }
        park(&server, &life);
    }
    if status != EXIT_HUNG {
        service.shutdown();
        flush_notes(&mut service);
    }
    status
}
