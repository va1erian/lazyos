//! An xui app's taskbar tray item (docs/tray-plan.md section 5.1), over
//! `libs/trayclient`.
//!
//! ```ignore
//! let mut tray = TrayIcon::new();
//! tray.set(trayclient::item(trayclient::lucide("volume-2"), "Volume 40%"))?;
//! // on a UI timer:
//! for event in tray.poll() { /* Activate, MenuItem, Scroll, ... */ }
//! ```
//!
//! [`TrayIcon::poll`] never parks: it drains the item's event channel with
//! the channel counters first, and follows the shell's retained
//! `session/<s>/shell/tray` generation through a [`Follow`] (the thread's hub
//! thread reads it; the poll only empties a queue), setting the item again
//! when a restarted shell publishes a new one. An app therefore
//! keeps its icon across a shell restart without code of its own. A labelled
//! app needs `os.lazy.shell.tray.v1` and `subscribe:session/+/shell/tray` in
//! its manifest (both implied by `resident` from stage T3).

use crate::trace::{self, Channel};
use libmessenger::client::{Connection, Deadline, Retry};
use libmessenger::Parcel;
use trayclient::{wire, Event, Transport, Tray};

use crate::fabric::{pull, Feed, Watch};
use crate::platform::messenger::{Conn, Observed};
use crate::sys::{self, errno};

/// Ticks (100 Hz) one tray call may take: the shell answers from its
/// heartbeat, about every 30 ms.
const CALL_TICKS: u64 = 100;
/// Ticks between tries of a `Set` a restarting shell refused.
const RETRY_TICKS: u64 = 50;
/// Most events read per poll.
const PER_POLL: usize = 16;
/// Receive buffer for one event (the largest is `Activate`, a few dozen
/// bytes).
const EVENT_BYTES: usize = 512;

/// [`trayclient::Transport`] over this task's Messenger syscalls. `libs/trayclient` frames its own
/// requests (its `Transport` takes the method id, body and objects), so the shell is reached
/// through a raw [`Conn`], not a generated client.
struct XuiTransport {
    shell: Conn,
}

impl XuiTransport {
    fn new() -> XuiTransport {
        XuiTransport {
            shell: Connection::named(Observed::default(), trayclient::NAME.to_owned()),
        }
    }
}

impl Transport for XuiTransport {
    fn create_pair(&mut self) -> trayclient::Result<(u64, u64)> {
        sys::msg_create_pair()
    }

    fn call(
        &mut self,
        method: u32,
        body: Vec<u8>,
        objects: Vec<trayclient::Object>,
    ) -> trayclient::Result<Vec<u8>> {
        self.shell.try_connect().map_err(|_| -errno::ENOENT)?;
        self.shell
            .call(
                wire::INTERFACE_ID,
                method,
                body,
                objects,
                Deadline::ticks(CALL_TICKS),
                Retry::Never,
            )
            .map(|reply| reply.body)
            .map_err(i64::from)
    }

    fn close(&mut self, handle: u64) {
        let _ = sys::msg_close(handle);
    }
}

/// This app's tray item.
pub struct TrayIcon {
    tray: Tray<XuiTransport>,
    /// The shell's generation topic for this session (`None` when the
    /// session is unknown: then the item is never set again by itself).
    generation: Option<Feed<wire::Generation>>,
    /// When a refused `Set` is next tried again (a PIT tick).
    next_retry: u64,
    buf: Vec<u8>,
}

impl Default for TrayIcon {
    fn default() -> TrayIcon {
        TrayIcon::new()
    }
}

impl TrayIcon {
    /// No item yet; follows this task's session's tray generation.
    pub fn new() -> TrayIcon {
        let session = sys::cred_get(None).ok().map(|cred| cred.session);
        let generation = session.and_then(|session| {
            wire::subscribe_session_shell_tray(&mut Watch, &session.to_string()).ok()
        });
        TrayIcon {
            tray: Tray::new(XuiTransport::new()),
            generation,
            next_retry: 0,
            buf: vec![0; EVENT_BYTES],
        }
    }

    /// Show `item` (replacing the app's current one). On failure (no shell
    /// yet) the item is kept and set when the shell announces itself.
    pub fn set(&mut self, item: wire::Item) -> Result<(), i64> {
        // Learn the current generation first, so the item is registered
        // with it and not set twice when the retained value arrives.
        self.follow_generation(true);
        self.tray.set(item)
    }

    /// The channel the item's events arrive on, for an app that parks on it
    /// with others (`crate::resident`).
    pub fn channel(&self) -> Option<u64> {
        self.tray.events()
    }

    /// The doorbell of the generation feed, for the same park: ready when the
    /// shell published a generation ([`TrayIcon::poll`] reads it).
    pub fn generation_bell(&self) -> Option<u64> {
        self.generation
            .as_ref()
            .and_then(|stream| stream.handle().bell())
    }

    /// Change the given parts of the item.
    pub fn update(&mut self, patch: wire::UpdateArgs) -> Result<(), i64> {
        self.tray.update(patch)
    }

    /// Remove the item.
    pub fn clear(&mut self) -> Result<(), i64> {
        self.tray.clear()
    }

    /// The events that arrived since the last poll; never parks. Liveness
    /// pings are answered by being read and are not returned.
    pub fn poll(&mut self) -> Vec<Event> {
        self.follow_generation(false);
        let Some(channel) = self.tray.events() else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for _ in 0..PER_POLL {
            match sys::msg_queued(channel) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => {
                    // The shell's end is gone: it died or dropped the item.
                    self.tray.disconnected();
                    break;
                }
            }
            let Ok(result) = sys::msg_recv(channel, &mut self.buf, sys::EXPIRED_DEADLINE) else {
                break;
            };
            let Some(bytes) = self.buf.get(..result.bytes as usize) else {
                continue;
            };
            let Ok(parcel) = Parcel::decode(bytes) else {
                continue;
            };
            let header = &parcel.header;
            match trayclient::decode_event(header.interface_id, header.method, &parcel.body) {
                Some(Event::Ping) | None => {}
                Some(event) => found.push(event),
            }
        }
        found
    }

    /// Learn the shell's generation from what the hub queued; `set` asks
    /// first so the item is registered with the generation already known.
    fn follow_generation(&mut self, now: bool) {
        let Some(stream) = self.generation.as_mut() else {
            return;
        };
        let tick = sys::clock_ticks();
        let mut generations = Vec::new();
        pull(stream, |event| generations.push(event.value.generation));
        if !now && tick >= self.next_retry {
            // A `Set` a restarting shell refused is tried again.
            self.next_retry = tick.saturating_add(RETRY_TICKS);
            if let Some(Err(code)) = self.tray.retry() {
                trace::line!(Channel::Tray, "RESET:RETRY err={}", -code);
            }
        }
        for generation in generations {
            if let Some(Err(code)) = self.tray.generation(generation) {
                trace::line!(Channel::Tray, "RESET:FAIL err={}", -code);
            }
        }
    }
}
