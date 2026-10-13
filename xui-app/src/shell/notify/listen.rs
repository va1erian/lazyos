//! The apps' event channels (`os.lazy.notify.v1` `Listen`): what happened to
//! a sender's toasts goes to its own channel, one oneway message each. The
//! shell `Ping`s every channel about once a second; `EPIPE` means the app
//! went away, and its channel is forgotten (its toasts stay: a notice an app
//! posted just before it exited is still news).

use lazyshell::notify::{Event, EventKind};
use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::os_lazy_notify_events_v1 as wire;

use super::super::ctx::Ctx;
use crate::sys::{self, errno};

/// Ticks (100 Hz) between pings: about once a second, like the tray's.
const PING_TICKS: u64 = 100;
/// Most senders listening at once.
pub const LISTENERS_MAX: usize = 64;

/// Most senders whose app identity is remembered.
const SENDERS_MAX: usize = 128;

/// One sender's channel and the label its `Listen` came with.
struct Listener {
    owner: String,
    channel: u64,
}

/// Every sender's event channel, and the package label and session the kernel stamped on each
/// sender's calls (what a click on its toast brings forward: the shell raises the windows of that
/// app, whether or not it listens, and whichever thread of it made the call).
#[derive(Default)]
pub struct Listeners {
    rows: Vec<Listener>,
    senders: Vec<(String, u32, u64)>,
    next_ping: u64,
}

impl Listeners {
    /// Keep `channel` as `owner`'s, closing the one it replaces; `false`
    /// (the caller closes `channel`) when too many senders listen.
    pub fn keep(&mut self, owner: &str, channel: u64) -> bool {
        if let Some(index) = self.rows.iter().position(|row| row.owner == owner) {
            let old = self.rows.remove(index);
            let _ = sys::msg_close(old.channel);
        } else if self.rows.len() >= LISTENERS_MAX {
            return false;
        }
        self.rows.push(Listener {
            owner: owner.to_owned(),
            channel,
        });
        true
    }

    /// Remember the package label and session the kernel stamped on `owner`'s call. An
    /// unlabelled sender (a script) has nothing a window could be named by and is not kept.
    pub fn note_sender(&mut self, owner: &str, label: u32, session: u64) {
        if label == 0 {
            return;
        }
        match self.senders.iter_mut().find(|(name, ..)| name == owner) {
            Some(row) => (row.1, row.2) = (label, session),
            None => {
                if self.senders.len() >= SENDERS_MAX {
                    self.senders.remove(0);
                }
                self.senders.push((owner.to_owned(), label, session));
            }
        }
    }

    /// The label and session `owner` last called under.
    pub fn app_of(&self, owner: &str) -> Option<(u32, u64)> {
        self.senders
            .iter()
            .find(|(name, ..)| name == owner)
            .map(|(_, label, session)| (*label, *session))
    }

    /// Forget (and close) `owner`'s channel.
    pub fn drop_owner(&mut self, owner: &str) {
        if let Some(index) = self.rows.iter().position(|row| row.owner == owner) {
            let _ = sys::msg_close(self.rows.remove(index).channel);
        }
    }

    /// Ping every channel when due; the dead ones go.
    pub fn ping(&mut self, _ctx: &Ctx) {
        let now = sys::clock_ticks();
        if now < self.next_ping {
            return;
        }
        self.next_ping = now.saturating_add(PING_TICKS);
        let mut gone = Vec::new();
        for row in &self.rows {
            match send(row.channel, wire::METHOD_PING, Vec::new()) {
                // A full queue is a slow app, not a dead one.
                Ok(()) => {}
                Err(code) if code == -errno::EAGAIN => {}
                Err(_) => gone.push(row.owner.clone()),
            }
        }
        for owner in gone {
            println!("SHELL:NOTIFY:LISTEN:GONE owner={owner}");
            self.drop_owner(&owner);
        }
    }

    /// Send `event` to its owner's channel, if it listens.
    pub fn deliver(&self, _ctx: &Ctx, event: &Event) {
        let Some(row) = self.rows.iter().find(|row| row.owner == event.owner) else {
            return;
        };
        let (method, body) = match event.kind {
            // Shown is the shell's own business.
            EventKind::Shown => return,
            EventKind::Activated => (
                wire::METHOD_ACTIVATED,
                wire::encode_activated_args(&wire::ActivatedArgs { id: event.id }),
            ),
            EventKind::Action(action) => (
                wire::METHOD_ACTIONINVOKED,
                wire::encode_action_invoked_args(&wire::ActionInvokedArgs {
                    id: event.id,
                    action,
                }),
            ),
            EventKind::Closed(reason) => (
                wire::METHOD_CLOSED,
                wire::encode_closed_args(&wire::ClosedArgs {
                    id: event.id,
                    reason: reason.wire(),
                }),
            ),
        };
        let Ok(body) = body else {
            return;
        };
        if let Err(code) = send(row.channel, method, body) {
            // A busy app misses an event; it is not worth more than a log.
            println!(
                "SHELL:NOTIFY:EVENT:FAIL owner={} id={} err={}",
                event.owner, event.id, -code
            );
        }
    }
}

/// Send one oneway event on `channel`.
fn send(channel: u64, method: u32, body: Vec<u8>) -> Result<(), i64> {
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: wire::INTERFACE_ID,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects: Vec::new(),
    };
    sys::msg_send(channel, &parcel)
}
