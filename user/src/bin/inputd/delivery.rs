//! Getting events into session endpoints without losing any.
//!
//! A client's endpoint holds 64 messages and a typed character is three, so
//! a client descheduled for a moment (a busy first boot) fills it quickly.
//! What does not fit waits in the session's [`Outbox`] and goes out, in
//! order, on the next pass of the service loop ([`Delivery::flush_all`]),
//! and every newer event queues behind it. Only a client that stays stalled
//! past [`outbox::CAPACITY`] events loses them, and then explicitly:
//! `INPUTD:DROP session=<id> lost=<n>`, and the session is resynchronised
//! (`KeyboardLeave` + `KeyboardEnter` with the keys held now) before it gets
//! anything newer, so a dropped release cannot leave a key stuck. A backlog
//! that drained after reaching [`REPORT_PEAK`] events is reported once as
//! `INPUTD:BACKLOG session=<id> peak=<n>`.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::vec::Vec;

use inputmap::outbox::{self, Outbox, Pushed, Sent};
use user::messenger::input as api;
use user::messenger::input::wire;
use user::messenger::{errno, Endpoint, Error};
use user::sys;

/// Backlog peaks at or above this are worth a line once they drain.
const REPORT_PEAK: usize = 64;

/// One queued event: its method and encoded body.
type Event = (u32, Vec<u8>);

/// Whether a session can still be reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reach {
    Live,
    /// Its endpoint is gone: the caller tears the session down.
    Gone,
}

#[derive(Default)]
pub(super) struct Delivery {
    /// session -> the client's event endpoint.
    endpoints: BTreeMap<u64, Endpoint>,
    /// Sessions with events waiting (or a resync owed).
    outboxes: BTreeMap<u64, Outbox<Event>>,
}

impl Delivery {
    pub(super) fn insert(&mut self, session: u64, endpoint: Endpoint) {
        self.endpoints.insert(session, endpoint);
    }

    /// Close the session's endpoint and discard whatever it had waiting.
    pub(super) fn forget(&mut self, session: u64) {
        self.outboxes.remove(&session);
        if let Some(endpoint) = self.endpoints.remove(&session) {
            let _ = endpoint.close();
        }
    }

    /// Send `method` to `session`, behind any backlog it has. `enter` builds
    /// the `KeyboardEnter` body a resync needs (the keys held right now).
    pub(super) fn send(
        &mut self,
        session: u64,
        method: u32,
        body: Vec<u8>,
        enter: &dyn Fn() -> Option<Vec<u8>>,
    ) -> Reach {
        if !self.outboxes.contains_key(&session) {
            match self.transmit(session, method, &body) {
                Sent::Ok => return Reach::Live,
                Sent::Gone => return Reach::Gone,
                Sent::Full => {}
            }
        } else if self.flush(session, enter) == Reach::Gone {
            return Reach::Gone;
        }
        let outbox = self.outboxes.entry(session).or_default();
        if let Pushed::Dropped { lost } = outbox.push((method, body)) {
            sys::write_str(&format!(
                "INPUTD:DROP session={session} lost={lost} capacity={}\n",
                outbox::CAPACITY
            ));
        }
        Reach::Live
    }

    /// Retry every backlog. Returns the sessions found gone.
    pub(super) fn flush_all(&mut self, enter: &dyn Fn() -> Option<Vec<u8>>) -> Vec<u64> {
        let waiting: Vec<u64> = self.outboxes.keys().copied().collect();
        waiting
            .into_iter()
            .filter(|session| self.flush(*session, enter) == Reach::Gone)
            .collect()
    }

    /// Resynchronise `session` if it is owed one, then hand over its backlog
    /// until the client's endpoint is full again.
    fn flush(&mut self, session: u64, enter: &dyn Fn() -> Option<Vec<u8>>) -> Reach {
        let Some(mut outbox) = self.outboxes.remove(&session) else {
            return Reach::Live;
        };
        let mut result = Sent::Ok;
        if outbox.needs_resync() {
            result = self.resync(session, enter);
            if result == Sent::Ok {
                outbox.resynced();
            }
        }
        if result == Sent::Ok {
            result = outbox.flush(|(method, body)| self.transmit(session, *method, body));
        }
        match result {
            Sent::Gone => Reach::Gone,
            _ if outbox.is_clear() => {
                if outbox.peak() >= REPORT_PEAK {
                    sys::write_str(&format!(
                        "INPUTD:BACKLOG session={session} peak={}\n",
                        outbox.peak()
                    ));
                }
                Reach::Live
            }
            _ => {
                self.outboxes.insert(session, outbox);
                Reach::Live
            }
        }
    }

    /// `KeyboardLeave` then `KeyboardEnter`: to the client, a focus loss and
    /// regain, the recovery it already has. Both or nothing is retried.
    fn resync(&self, session: u64, enter: &dyn Fn() -> Option<Vec<u8>>) -> Sent {
        let Some(enter) = enter() else {
            return Sent::Full;
        };
        match self.transmit(session, wire::METHOD_KEYBOARDLEAVE, &[]) {
            Sent::Ok => {}
            other => return other,
        }
        // The Leave went out; an Enter that does not fit is retried as a
        // whole resync later, and a second Leave is harmless.
        self.transmit(session, wire::METHOD_KEYBOARDENTER, &enter)
    }

    fn transmit(&self, session: u64, method: u32, body: &[u8]) -> Sent {
        let Some(endpoint) = self.endpoints.get(&session) else {
            return Sent::Gone;
        };
        match endpoint.send(&api::event(api::INTERFACE, method, body.to_vec())) {
            Ok(()) => Sent::Ok,
            Err(Error::Errno(code)) if code == -errno::EPIPE => Sent::Gone,
            // A full inbox or the sender's queue quota: try again later.
            Err(_) => Sent::Full,
        }
    }
}
