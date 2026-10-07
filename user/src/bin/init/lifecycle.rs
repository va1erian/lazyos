//! A launched app's lifecycle (docs/tray-plan.md section 5): the
//! `os.lazy.init.app.v1` line an app asks for with `Watch`, the `Reopen`s a
//! second launch of a running resident app becomes, and the graceful quit a
//! `Stop` starts.
//!
//! * **Watch** is served on its own name ([`APP_NAME`]) so an app can be
//!   granted it without `Launch`, `Stop` or `Shutdown`. Only the running task
//!   of a launched row may call, for its own row (`ESRCH` otherwise). The
//!   `Reopen`s queued before it are sent at once, in order; a quit already
//!   asked for is sent with the grace left.
//! * **Reopen.** A launch of a resident app already running in the session
//!   starts nothing: its args reach the instance as `Reopen`, or wait in the
//!   row's queue (at most [`REOPEN_QUEUE`], the oldest dropped) until it
//!   watches.
//! * **Quit.** A `Stop` of an app that watches, or of a resident app that
//!   may still come to watch, sends `Quit(grace)` and leaves it
//!   [`GRACE_TICKS`] (a hard 3 s counted from the `Stop`) before the kill;
//!   any other app is killed at once, as before. The `Stop` itself is
//!   answered only once every target has been reaped ([`Stops`]).
//!
//! Serial: `INIT:APP:WATCH app=<id> pid=<n>`, `INIT:APP:REOPEN:SENT|QUEUED
//! app=<id>`, `INIT:APP:REOPEN:DROPPED app=<id>`, `INIT:APP:QUIT:SENT
//! app=<id> grace_ms=<n>`, `INIT:APP:QUIT:TIMEOUT app=<id> pid=<n>`,
//! `INIT:STOP:DONE app=<id> stopped=<n>`.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{flags, Header, VERSION};
use messenger_generated::os_lazy_init_app_events_v1 as events;
use messenger_generated::os_lazy_init_app_v1 as app_wire;
use user::messenger::{self, services, Endpoint, Message, Parcel};
use user::sys;

use super::service::{Phase, Service};

/// The interface served on [`APP_NAME`].
pub(super) const APP_INTERFACE: u64 = app_wire::INTERFACE_ID;
/// The name `os.lazy.init.app.v1` is served under.
pub(super) const APP_NAME: &str = "os.lazy.init.app";
/// The quit grace: 3 s at 100 Hz, for every app (no per-package override).
pub(super) const GRACE_TICKS: u64 = 300;
/// Most `Reopen`s queued for an instance that does not watch yet.
pub(super) const REOPEN_QUEUE: usize = 16;
/// Milliseconds per PIT tick.
const MS_PER_TICK: u64 = 10;

/// One row's lifecycle state.
#[derive(Default)]
pub(super) struct Lifecycle {
    /// The package declared `resident`: single instance, always in the tray.
    pub(super) resident: bool,
    /// The channel the running instance asked for with `Watch`.
    watch: Option<Endpoint>,
    /// `Reopen` arguments waiting for the first `Watch`.
    reopens: VecDeque<String>,
    /// A `Stop` asked this run to quit: `stop_deadline` ends its grace.
    pub(super) quit: bool,
}

impl Lifecycle {
    pub(super) fn resident(resident: bool) -> Lifecycle {
        Lifecycle {
            resident,
            ..Lifecycle::default()
        }
    }

    /// A new run starts: nothing of the old one carries over.
    pub(super) fn reset(&mut self) {
        if let Some(watch) = self.watch.take() {
            let _ = watch.release();
        }
        self.reopens.clear();
        self.quit = false;
    }

    /// A supervised restart of the same row: the old run's channel and quit
    /// go, but `Reopen`s queued while it was down (a launch during the
    /// backoff) wait for the new run's `Watch`.
    pub(super) fn respawn(&mut self) {
        if let Some(watch) = self.watch.take() {
            let _ = watch.release();
        }
        self.quit = false;
    }

    /// Whether the instance watches its lifecycle.
    pub(super) fn watching(&self) -> bool {
        self.watch.is_some()
    }
}

/// A oneway event parcel on the lifecycle channel.
fn event(method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: events::INTERFACE_ID,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        ..Parcel::default()
    }
}

/// Send `Reopen(args)` on `watch`.
fn send_reopen(watch: &Endpoint, args: &str) -> bool {
    let Ok(body) = events::encode_reopen_args(&events::ReopenArgs {
        args: String::from(args),
    }) else {
        return false;
    };
    watch.send(&event(events::METHOD_REOPEN, body)).is_ok()
}

/// Send `Quit(grace_ms)` on `watch`.
fn send_quit(watch: &Endpoint, grace_ms: u32) -> bool {
    let Ok(body) = events::encode_quit_args(&events::QuitArgs { grace_ms }) else {
        return false;
    };
    watch.send(&event(events::METHOD_QUIT, body)).is_ok()
}

/// The grace left before `deadline`, in milliseconds.
fn grace_left(deadline: u64, now: u64) -> u32 {
    let ms = deadline.saturating_sub(now).saturating_mul(MS_PER_TICK);
    u32::try_from(ms).unwrap_or(u32::MAX)
}

/// `Watch`: adopt the transferred channel as the sender's row's lifecycle
/// line, then deliver what waited for it.
pub(super) fn watch(services: &mut [Service], message: &Message) -> messenger::Result<Parcel> {
    let not_mine = messenger::Error::Errno(-messenger::errno::ESRCH);
    if message.method() != app_wire::METHOD_WATCH {
        release_handle(message);
        return Err(messenger::Error::Errno(-messenger::errno::EINVAL));
    }
    let Some(row) = services.iter_mut().find(|row| {
        row.launched
            && row.pid == message.sender
            && message.sender != 0
            && matches!(row.phase, Phase::Running | Phase::Stopping)
    }) else {
        release_handle(message);
        return Err(not_mine);
    };
    if message.handles == 0 {
        return Err(messenger::Error::Errno(-messenger::errno::EINVAL));
    }
    let channel = Endpoint::from_raw(message.first_handle);
    if let Some(old) = row.life.watch.replace(channel) {
        let _ = old.release();
    }
    sys::write_str(&format!(
        "INIT:APP:WATCH app={} pid={}\n",
        row.name, row.pid
    ));
    while let Some(args) = row.life.reopens.pop_front() {
        if send_reopen(&channel, &args) {
            sys::write_str(&format!("INIT:APP:REOPEN:SENT app={} queued=1\n", row.name));
        }
    }
    if row.life.quit && row.phase == Phase::Stopping {
        let grace = grace_left(row.stop_deadline, sys::clock());
        if send_quit(&channel, grace) {
            sys::write_str(&format!(
                "INIT:APP:QUIT:SENT app={} grace_ms={grace}\n",
                row.name
            ));
        }
    }
    Ok(reply(app_wire::METHOD_WATCH))
}

/// An empty reply to `method` of `os.lazy.init.app.v1` (a default parcel has
/// no version, which the kernel refuses as malformed).
fn reply(method: u32) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: app_wire::INTERFACE_ID,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        ..Parcel::default()
    }
}

/// Close a channel a refused `Watch` carried, so a caller cannot fill this
/// task's handle table.
fn release_handle(message: &Message) {
    if message.handles > 0 {
        let _ = Endpoint::from_raw(message.first_handle).release();
    }
}

/// A launch of the resident app running as `row`: hand `args` to the
/// instance now, or queue it until the instance watches.
pub(super) fn reopen(row: &mut Service, args: &str) {
    let name = row.name;
    if let Some(watch) = row.life.watch {
        if send_reopen(&watch, args) {
            sys::write_str(&format!("INIT:APP:REOPEN:SENT app={name}\n"));
            return;
        }
    }
    if row.life.reopens.len() >= REOPEN_QUEUE {
        row.life.reopens.pop_front();
        sys::write_str(&format!("INIT:APP:REOPEN:DROPPED app={name}\n"));
    }
    row.life.reopens.push_back(String::from(args));
    sys::write_str(&format!("INIT:APP:REOPEN:QUEUED app={name}\n"));
}

/// Whether a `Stop` of `row` gives it a grace rather than an instant kill:
/// it watches, or it is resident and may still come to watch.
pub(super) fn graceful(row: &Service) -> bool {
    row.life.watching() || row.life.resident
}

/// Start `row`'s graceful quit at `now`: `Quit` now if it watches, else at
/// its `Watch`; killed at the end of the grace either way.
pub(super) fn begin_quit(row: &mut Service, now: u64) {
    row.phase = Phase::Stopping;
    row.stop_deadline = now.saturating_add(GRACE_TICKS);
    row.killed = false;
    row.life.quit = true;
    row.life.reopens.clear();
    let grace = grace_left(row.stop_deadline, now);
    match row.life.watch {
        Some(watch) if send_quit(&watch, grace) => sys::write_str(&format!(
            "INIT:APP:QUIT:SENT app={} grace_ms={grace}\n",
            row.name
        )),
        _ => sys::write_str(&format!(
            "INIT:APP:QUIT:HELD app={} grace_ms={grace}\n",
            row.name
        )),
    }
}

/// Kill every quitting row whose grace ended (outside a shutdown, which
/// sweeps its own `Stopping` rows).
pub(super) fn sweep(services: &mut [Service], now: u64) {
    for row in services.iter_mut() {
        let due = row.life.quit && row.phase == Phase::Stopping && !row.killed;
        if !due || now < row.stop_deadline || row.pid == 0 {
            continue;
        }
        sys::write_str(&format!(
            "INIT:APP:QUIT:TIMEOUT app={} pid={}\n",
            row.name, row.pid
        ));
        row.killed = true;
        if let Err(code) = sys::kill(row.pid, sys::SIG_KILL) {
            sys::write_str(&format!(
                "INIT:STOP:KILL:FAIL app={} pid={} errno={code}\n",
                row.name, row.pid
            ));
        }
    }
}

/// The next grace end the supervisor must wake for.
pub(super) fn next_deadline(services: &[Service]) -> Option<u64> {
    services
        .iter()
        .filter(|row| row.life.quit && row.phase == Phase::Stopping && !row.killed)
        .map(|row| row.stop_deadline)
        .min()
}

/// One `Stop` waiting for its targets' exits.
struct Pending {
    txn: u64,
    app: String,
    pids: Vec<u64>,
    stopped: u64,
}

/// The `Stop` transactions held until every target has been reaped:
/// `init` is one task, so it keeps the transaction instead of blocking.
pub(super) struct Stops {
    server: Endpoint,
    pending: Vec<Pending>,
}

impl Stops {
    pub(super) fn new(server: Endpoint) -> Stops {
        Stops {
            server,
            pending: Vec::new(),
        }
    }

    /// Answer `txn` once `pids` have all exited (at once when none is left).
    pub(super) fn hold(&mut self, txn: u64, app: &str, pids: Vec<u64>, stopped: u64) {
        let pending = Pending {
            txn,
            app: String::from(app),
            pids,
            stopped,
        };
        if pending.pids.is_empty() {
            self.answer(&pending);
        } else {
            self.pending.push(pending);
        }
    }

    /// Task `pid` was reaped: answer every `Stop` it was the last target of.
    pub(super) fn reaped(&mut self, pid: u64) {
        if self.pending.is_empty() {
            return;
        }
        for pending in self.pending.iter_mut() {
            pending.pids.retain(|&target| target != pid);
        }
        let (done, waiting): (Vec<Pending>, Vec<Pending>) = core::mem::take(&mut self.pending)
            .into_iter()
            .partition(|pending| pending.pids.is_empty());
        self.pending = waiting;
        for pending in &done {
            self.answer(pending);
        }
    }

    fn answer(&self, pending: &Pending) {
        sys::write_str(&format!(
            "INIT:STOP:DONE app={} stopped={}\n",
            pending.app, pending.stopped
        ));
        if let Ok(reply) = services::stop_reply(pending.stopped) {
            let _ = self.server.reply_or_drop(pending.txn, &reply);
        }
    }
}
