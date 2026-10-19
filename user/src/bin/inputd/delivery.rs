//! Getting events into session endpoints without losing any.
//!
//! A client's endpoint holds 64 messages and a typed character is three, so
//! a client descheduled for a moment (a busy first boot) fills it quickly.
//! What does not fit waits in the session's [`Outbox`] and goes out, in
//! order, on the next pass of the service loop ([`Delivery::flush_all`]),
//! and every newer event queues behind it. Only a client that stays stalled
//! past [`outbox::CAPACITY`] events loses them, and then explicitly:
//! `INPUTD:DROP session=<id> lost=<n>`, and the session is resynchronised
//! (`KeyboardLeave` + `KeyboardEnter` with the keys held now, and the pad
//! pair if it opted in) before it gets
//! anything newer, so a dropped release cannot leave a key stuck. A backlog
//! that drained after reaching [`REPORT_PEAK`] events is reported once as
//! `INPUTD:BACKLOG session=<id> peak=<n>`.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use user::trace;

use inputmap::outbox::{self, Outbox, Pushed, Sent};
use user::messenger::input as api;
use user::messenger::input::wire;
use user::messenger::{errno, Endpoint, Error};

/// Backlog peaks at or above this are worth a line once they drain.
const REPORT_PEAK: usize = 64;

/// What a resynchronisation needs to know about the hub.
pub(super) trait Resync {
    /// The `KeyboardEnter` body for the keys held right now.
    fn keyboard_enter(&self) -> Option<Vec<u8>>;
    /// Whether `session` opted into pad events.
    fn pad_events(&self, session: u64) -> bool;
    /// Show `session`'s pad page live (the pads) or cleared.
    fn pad_page(&self, session: u64, live: bool);
}

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

    /// Send `method` to `session`, behind any backlog it has. `resync` says
    /// what a resync needs (the keys held right now, the pad page).
    pub(super) fn send(
        &mut self,
        session: u64,
        method: u32,
        body: Vec<u8>,
        resync: &dyn Resync,
    ) -> Reach {
        if !self.outboxes.contains_key(&session) {
            match self.transmit(session, method, &body) {
                Sent::Ok => return Reach::Live,
                Sent::Gone => return Reach::Gone,
                Sent::Full => {}
            }
        } else if self.flush(session, resync) == Reach::Gone {
            return Reach::Gone;
        }
        let outbox = self.outboxes.entry(session).or_default();
        if let Pushed::Dropped { lost } = outbox.push((method, body)) {
            trace::line!(
                trace::Channel::Inputd,
                "DROP session={session} lost={lost} capacity={}",
                outbox::CAPACITY
            );
        }
        Reach::Live
    }

    /// Whether any session has events waiting for room.
    pub(super) fn backlogged(&self) -> bool {
        !self.outboxes.is_empty()
    }

    /// Retry every backlog. Returns the sessions found gone.
    pub(super) fn flush_all(&mut self, resync: &dyn Resync) -> Vec<u64> {
        let waiting: Vec<u64> = self.outboxes.keys().copied().collect();
        waiting
            .into_iter()
            .filter(|session| self.flush(*session, resync) == Reach::Gone)
            .collect()
    }

    /// Resynchronise `session` if it is owed one, then hand over its backlog
    /// until the client's endpoint is full again.
    fn flush(&mut self, session: u64, resync: &dyn Resync) -> Reach {
        let Some(mut outbox) = self.outboxes.remove(&session) else {
            return Reach::Live;
        };
        let mut result = Sent::Ok;
        if outbox.needs_resync() {
            result = self.resync(session, resync);
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
                    trace::line!(
                        trace::Channel::Inputd,
                        "BACKLOG session={session} peak={}",
                        outbox.peak()
                    );
                }
                Reach::Live
            }
            _ => {
                self.outboxes.insert(session, outbox);
                Reach::Live
            }
        }
    }

    /// `KeyboardLeave` then `KeyboardEnter` and, for a session that opted
    /// into pads, `GamepadLeave` then `GamepadEnter`: to the client, a focus
    /// loss and regain, the recovery it already has (it releases what it held
    /// and seeds itself from the key and pad pages). The pad page is cleared
    /// before the Leave and live again before the Enter, as on a real focus
    /// change. Everything is retried as a whole.
    fn resync(&self, session: u64, resync: &dyn Resync) -> Sent {
        let Some(enter) = resync.keyboard_enter() else {
            return Sent::Full;
        };
        match self.transmit(session, wire::METHOD_KEYBOARDLEAVE, &[]) {
            Sent::Ok => {}
            other => return other,
        }
        // The Leave went out; an Enter that does not fit is retried as a
        // whole resync later, and a second Leave is harmless.
        match self.transmit(session, wire::METHOD_KEYBOARDENTER, &enter) {
            Sent::Ok => {}
            other => return other,
        }
        // Pad events may be among what was lost; a session that never asked
        // for them is not told of them.
        if !resync.pad_events(session) {
            return Sent::Ok;
        }
        resync.pad_page(session, false);
        match self.transmit(session, wire::METHOD_GAMEPADLEAVE, &[]) {
            Sent::Ok => {}
            other => return other,
        }
        resync.pad_page(session, true);
        self.transmit(session, wire::METHOD_GAMEPADENTER, &[])
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
