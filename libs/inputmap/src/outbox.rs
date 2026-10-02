//! Per-session delivery backlog: key content is never dropped silently.
//!
//! `inputd` sends every event to a client's own Messenger endpoint, whose
//! inbox holds at most 64 messages. A typed character is three of them
//! (`KeyEvent` down, `TextInput`, `KeyEvent` up), so a client that is not
//! scheduled for a fifth of a second (a busy first boot: `pkgd` installing the
//! core packages, the compositor reloading its menu) fills its inbox after
//! about twenty characters. `inputd` used to discard whatever did not fit and
//! resynchronise the client afterwards, which lost the middle of whatever was
//! being typed with no trace on serial.
//!
//! An [`Outbox`] keeps what did not fit, in order, and [`Outbox::flush`] hands
//! it over as the client drains. It is bounded ([`CAPACITY`] events, about 340
//! typed characters): past that the client is not draining at all, and the
//! whole backlog is discarded *explicitly* — the caller logs the count and
//! resynchronises the client (`KeyboardLeave` + `KeyboardEnter`) before it
//! gets anything newer, so a lost release cannot leave a key stuck.
//!
//! Generic over the queued item so the policy is host-tested without
//! Messenger; `inputd` queues `(method, body)` pairs.

use alloc::collections::VecDeque;

/// Events one session may have waiting before its backlog is dropped.
pub const CAPACITY: usize = 1024;

/// What a send attempt reported, as seen by [`Outbox::flush`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    /// Delivered.
    Ok,
    /// The client's inbox (or its sender's queue quota) is full: keep the
    /// item and try again later.
    Full,
    /// The client is gone: stop, the caller tears the session down.
    Gone,
}

/// What [`Outbox::push`] did with an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    /// Queued behind the existing backlog.
    Queued,
    /// The backlog was full: it and the new item were discarded, `lost`
    /// events in all. The session must be resynchronised before the next
    /// delivery ([`Outbox::needs_resync`]).
    Dropped { lost: usize },
}

/// One session's pending events.
#[derive(Debug)]
pub struct Outbox<T> {
    pending: VecDeque<T>,
    /// Events discarded since the session was last resynchronised.
    lost: usize,
    /// Most events ever waiting at once (diagnostics).
    peak: usize,
}

impl<T> Default for Outbox<T> {
    fn default() -> Self {
        Outbox::new()
    }
}

impl<T> Outbox<T> {
    pub const fn new() -> Self {
        Outbox {
            pending: VecDeque::new(),
            lost: 0,
            peak: 0,
        }
    }

    /// Nothing waiting and nothing lost: a new event may go straight out.
    pub fn is_clear(&self) -> bool {
        self.pending.is_empty() && self.lost == 0
    }

    /// Events waiting.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Most events that were ever waiting at once.
    pub fn peak(&self) -> usize {
        self.peak
    }

    /// Events were discarded: the client must see `KeyboardLeave` +
    /// `KeyboardEnter` before anything newer.
    pub fn needs_resync(&self) -> bool {
        self.lost > 0
    }

    /// Events discarded since the last [`Outbox::resynced`].
    pub fn lost(&self) -> usize {
        self.lost
    }

    /// The caller delivered the resynchronisation.
    pub fn resynced(&mut self) {
        self.lost = 0;
    }

    /// Queue `item` behind the backlog, or drop everything when it is full.
    pub fn push(&mut self, item: T) -> Pushed {
        if self.pending.len() >= CAPACITY {
            let lost = self.pending.len() + 1;
            self.pending.clear();
            self.lost = self.lost.saturating_add(lost);
            return Pushed::Dropped { lost };
        }
        self.pending.push_back(item);
        self.peak = self.peak.max(self.pending.len());
        Pushed::Queued
    }

    /// Hand waiting items to `send` in order until it reports `Full` or
    /// `Gone`, or the backlog is empty. Returns the first non-`Ok` result
    /// (`Ok` when everything went out). The item that did not fit stays first.
    pub fn flush(&mut self, mut send: impl FnMut(&T) -> Sent) -> Sent {
        while let Some(item) = self.pending.front() {
            match send(item) {
                Sent::Ok => {
                    self.pending.pop_front();
                }
                other => return other,
            }
        }
        Sent::Ok
    }
}
