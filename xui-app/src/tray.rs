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
//! `session/<s>/shell/tray` generation through a [`TopicFeed`], setting the
//! item again when a restarted shell publishes a new one. An app therefore
//! keeps its icon across a shell restart without code of its own. A labelled
//! app needs `os.lazy.shell.tray.v1` and `subscribe:session/+/shell/tray` in
//! its manifest (both implied by `resident` from stage T3).

use libmessenger::Parcel;
use messenger_generated::os_lazy_messenger_topics_v1 as topics;
use trayclient::{wire, Event, Transport, Tray};

use crate::platform::messenger::Service;
use crate::platform::topic_feed::TopicFeed;
use crate::server::ERROR_FIELD;
use crate::sys::{self, errno};

/// Ticks (100 Hz) one tray call may take: the shell answers from its
/// heartbeat, about every 30 ms.
const CALL_TICKS: u64 = 100;
/// Ticks between looks at the generation topic.
const GENERATION_TICKS: u64 = 50;
/// How long the first look at the generation may wait for its retained
/// value.
const FIRST_LOOK_TICKS: u64 = 20;
/// Most events read per poll.
const PER_POLL: usize = 16;
/// Receive buffer for one event (the largest is `Activate`, a few dozen
/// bytes).
const EVENT_BYTES: usize = 512;

/// [`trayclient::Transport`] over this task's Messenger syscalls.
struct XuiTransport;

impl Transport for XuiTransport {
    fn create_pair(&mut self) -> trayclient::Result<(u64, u64)> {
        sys::msg_create_pair()
    }

    fn call(
        &mut self,
        method: u32,
        body: Vec<u8>,
        handles: Vec<u64>,
    ) -> trayclient::Result<Vec<u8>> {
        let shell = Service::try_connect(trayclient::NAME).ok_or(-errno::ENOENT)?;
        shell
            .call_moving_within(
                wire::INTERFACE_ID,
                method,
                ERROR_FIELD,
                body,
                handles,
                CALL_TICKS,
            )
            .map(|reply| reply.body)
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
    generation: Option<TopicFeed>,
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
        let generation = session
            .and_then(|session| wire::name_session_shell_tray(&session.to_string()).ok())
            .map(|topic| TopicFeed::new(topic, topics::QOS_LATEST, 1, GENERATION_TICKS));
        TrayIcon {
            tray: Tray::new(XuiTransport),
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

    fn follow_generation(&mut self, now: bool) {
        let Some(feed) = self.generation.as_mut() else {
            return;
        };
        let tick = sys::clock_ticks();
        if !now && tick >= self.next_retry {
            // A `Set` a restarting shell refused is tried again.
            self.next_retry = tick.saturating_add(GENERATION_TICKS);
            if let Some(Err(code)) = self.tray.retry() {
                println!("TRAY:RESET:RETRY err={}", -code);
            }
        }
        let events = if now {
            feed.poll_waiting(FIRST_LOOK_TICKS)
        } else {
            feed.poll()
        };
        for event in events {
            if let Ok(value) = wire::decode_session_shell_tray(&event.payload) {
                if let Some(Err(code)) = self.tray.generation(value.generation) {
                    println!("TRAY:RESET:FAIL err={}", -code);
                }
            }
        }
    }
}
