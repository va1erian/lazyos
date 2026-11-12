//! A followed topic: the typed stream, its doorbell and its reconnect, in one
//! place for every native service that reads a topic.
//!
//! A [`Feed`] is made with the generated `subscribe_<topic>` helper of the
//! topic it follows. It never waits on the broker and never reads on a timer:
//!
//! * [`Feed::ensure`] makes the stream when it is missing, with one registry
//!   lookup (the broker may not be up yet) and every call bounded, at most
//!   once per [`RETRY_TICKS`]; a service that has nothing else to wake it
//!   asks [`Feed::next_retry`] for the one timed wake that ends with the
//!   first success;
//! * the doorbell ([`Feed::bell`]) is what a loop parks on beside its other
//!   endpoints; [`Feed::drain`] reads what rang, to the end, which re-arms
//!   the bell, and a stream just made is read once without a ring (its
//!   retained replay was queued before the doorbell existed);
//! * a service that is not parked on the bell but reads at moments of its own
//!   ([`Feed::drain_rung`]) asks the bell instead: that is a local message
//!   queue, never a call to the broker, so looking is cheap at any rate.
//!
//! A busy broker (`EAGAIN`) keeps the stream, read again at the next drain.
//! A transport failure (the broker went away) drops the stream, and it is
//! made again, replay included.

use alloc::vec::Vec;

use messenger_generated::topics::{Event, NextError, Subscription};

use super::Bus;
use crate::messenger::{errno, Endpoint, Error, Result};
use crate::sys;

/// Ticks (100 Hz) between attempts to make a missing stream.
pub const RETRY_TICKS: u64 = 50;
/// Ticks one call to the broker may take.
const BOUND_TICKS: u64 = 50;

/// Whether the broker refused a pull as busy, a transient state.
fn is_busy(error: &Error) -> bool {
    error.errno() == Some(-errno::EAGAIN)
}

/// The typed stream a [`Feed`] holds.
pub type Stream<T> = Subscription<super::Subscription, T>;

/// One followed topic.
pub struct Feed<T> {
    /// The generated `subscribe_*` helper, with the topic's arguments filled.
    open: fn(&mut Bus) -> Result<Stream<T>>,
    stream: Option<Stream<T>>,
    /// The broker, once found; dropped (and so released) when a subscribe
    /// fails, so the next attempt looks it up again.
    bus: Option<Bus>,
    /// Made since the last drain.
    fresh: bool,
    next_try: u64,
}

impl<T> Feed<T> {
    /// A feed that follows what `open` subscribes to.
    pub const fn new(open: fn(&mut Bus) -> Result<Stream<T>>) -> Feed<T> {
        Feed {
            open,
            stream: None,
            bus: None,
            fresh: false,
            next_try: 0,
        }
    }

    /// Whether the stream is not there (yet).
    pub fn missing(&self) -> bool {
        self.stream.is_none()
    }

    /// The tick to wake at to try again; `None` once the stream exists.
    pub fn next_retry(&self) -> Option<u64> {
        self.missing().then_some(self.next_try)
    }

    /// Make the stream if it is missing and its time has come. Returns whether
    /// it was made now.
    pub fn ensure(&mut self) -> bool {
        let now = sys::clock();
        // Without a broker the attempt is one registry lookup, so it is made
        // on every call (a service that just came up is found at once); the
        // pause is for a subscribe the broker refused or timed out.
        if self.stream.is_some() || (self.bus.is_some() && now < self.next_try) {
            return false;
        }
        self.next_try = now + RETRY_TICKS;
        if self.bus.is_none() {
            // One lookup, never a wait: the broker may not be up yet.
            let Ok(bus) = Bus::connect() else {
                return false;
            };
            self.bus = Some(bus.bounded(BOUND_TICKS));
        }
        let Some(bus) = self.bus.as_mut() else {
            return false;
        };
        match (self.open)(bus) {
            Ok(stream) => {
                self.stream = Some(stream);
                self.fresh = true;
                true
            }
            Err(_) => {
                // A stale endpoint (the broker restarted) is looked up again.
                self.bus = None;
                false
            }
        }
    }

    /// The doorbell to park on, once the stream exists.
    pub fn bell(&mut self) -> Option<Endpoint> {
        self.stream.as_mut()?.handle_mut().bell().ok()
    }

    /// Whether [`Feed::drain`] has something to read without a ring: the
    /// stream was just made.
    pub fn fresh(&self) -> bool {
        self.stream.is_some() && self.fresh
    }

    /// Read what is queued when `rang` (the wait said the doorbell has a
    /// message) or the stream is fresh, handing each decoded event to
    /// `apply`. A payload that does not decode is skipped.
    pub fn drain(&mut self, rang: bool, apply: impl FnMut(Event<T>)) {
        if rang {
            if let Some(stream) = &self.stream {
                // The wait said a message is there, so this does not park.
                stream.handle().take_ring();
            }
        }
        self.read(rang, apply);
    }

    /// Read what is queued now, ring or no ring, to the end (which re-arms the
    /// doorbell), the last pull waiting up to `ticks` for an event. For a
    /// reader that must not act before a record it knows was published: the
    /// broker rings its bell *after* it answers the publisher, so a request
    /// the publisher sent next can arrive first, and a poll (an expired
    /// deadline) can miss an event a busy broker has queued. Costs up to
    /// `ticks` when nothing is queued; use it on a request, never on a timer.
    pub fn drain_waiting(&mut self, ticks: u64, apply: impl FnMut(Event<T>)) {
        if let Some(stream) = &mut self.stream {
            stream.handle_mut().set_wait_ticks(ticks);
        }
        self.read(true, apply);
        if let Some(stream) = &mut self.stream {
            stream.handle_mut().set_wait_ticks(0);
        }
    }

    /// Read to the end when `rang` or the stream is fresh (the ring, if any,
    /// is already consumed).
    fn read(&mut self, rang: bool, mut apply: impl FnMut(Event<T>)) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        if !rang && !self.fresh {
            return;
        }
        self.fresh = false;
        loop {
            match stream.next() {
                Ok(Some(event)) => apply(event),
                Ok(None) => return,
                Err(NextError::Decode(_)) => {}
                // The broker is busy (`EAGAIN`), not gone: keep the stream and
                // read again at the next drain.
                Err(NextError::Transport(error)) if is_busy(&error) => {
                    self.fresh = true;
                    return;
                }
                Err(NextError::Transport(_)) => {
                    self.stream = None;
                    return;
                }
            }
        }
    }

    /// [`Feed::drain`] for a reader that is not parked on the bell: it asks
    /// the doorbell itself (a local queue, not the broker).
    pub fn drain_rung(&mut self, apply: impl FnMut(Event<T>)) {
        let rang = self.stream.as_mut().is_some_and(|stream| {
            let attached = stream.handle_mut().bell().is_ok();
            // No doorbell to ask: read anyway rather than never.
            !attached || stream.handle().rung()
        });
        self.read(rang, apply);
    }

    /// Every event of a drain, collected (for a caller that must act on the
    /// batch while holding something the callback could not borrow).
    pub fn drain_all(&mut self, rang: bool) -> Vec<Event<T>> {
        let mut events = Vec::new();
        self.drain(rang, |event| events.push(event));
        events
    }
}
