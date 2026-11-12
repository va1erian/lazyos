//! One subscription's QoS queue and its byte accounting. Every push and pop
//! goes through here so [`Subscription::bytes`] is always what the queues
//! hold; the broker keeps the per-owner ledger from its changes.
//!
//! A queue never refuses its publisher. When the owner's byte budget has no
//! room for an event, the queue sheds its own oldest events (counted as
//! drops, like any QoS overflow) and, if even an empty queue cannot hold the
//! event, drops the new one.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use messenger_generated::os_lazy_messenger_topics_v1::Event;

use crate::filter::Filter;
use crate::qos::Qos;

/// What one queued event costs the broker: its payload, its topic and a flat
/// allowance for the record itself.
pub fn event_cost(event: &Event) -> usize {
    event.payload.len() + event.topic.len() + 64
}

/// `total - amount`, loudly: a byte count that goes negative is accounting
/// drift, which fails a debug build (and every host test) instead of being
/// clamped away.
pub(crate) fn take_bytes(total: usize, amount: usize) -> usize {
    match total.checked_sub(amount) {
        Some(left) => left,
        None => {
            debug_assert!(false, "byte accounting underflow: {total} - {amount}");
            0
        }
    }
}

/// One live subscription with its QoS queue and counters. `B` is the owner's
/// doorbell handle (an endpoint in `messengerd`).
pub struct Subscription<B> {
    pub id: u64,
    /// Task slot the subscribing call came from (kernel-stamped).
    pub owner: u64,
    /// Whether the owner is a platform task (no label, no session): it is
    /// charged against the larger system budget with the reserve behind it.
    pub platform: bool,
    pub filter: Filter,
    pub qos: Qos,
    /// Events ready for delivery (latest / buffered / reliable).
    queue: VecDeque<Event>,
    /// Coalesced per-publisher slots (conflate only).
    conflated: Vec<(u64, Event)>,
    /// Bytes the two queues hold ([`event_cost`] of each event).
    pub bytes: usize,
    pub drops: u64,
    pub delivered: u64,
    pub matched: u64,
    /// The owner's doorbell (`Bell`, P7.2): this end of its channel.
    pub bell: Option<B>,
    /// Whether the bell rang since the owner last found the queue empty.
    pub rung: bool,
}

impl<B> Subscription<B> {
    pub(crate) fn new(id: u64, owner: u64, platform: bool, filter: Filter, qos: Qos) -> Self {
        Subscription {
            id,
            owner,
            platform,
            filter,
            qos,
            queue: VecDeque::new(),
            conflated: Vec::new(),
            bytes: 0,
            drops: 0,
            delivered: 0,
            matched: 0,
            bell: None,
            rung: false,
        }
    }

    /// Pending events across whichever queue the QoS uses.
    pub fn queued(&self) -> u64 {
        (self.queue.len() + self.conflated.len()) as u64
    }

    /// The next event, without consuming it: the pop happens in
    /// [`Subscription::retire`] once the reply carrying the event has
    /// reached the subscriber. Borrowed, so a poll does not copy a payload.
    pub fn peek(&self) -> Option<&Event> {
        match self.qos {
            Qos::Conflate => self.conflated.first().map(|(_, event)| event),
            _ => self.queue.front(),
        }
    }

    /// Pop the head when it is event `sequence` (a late commit cannot pop a
    /// newer event).
    pub(crate) fn retire(&mut self, sequence: u64) {
        if self.peek().is_some_and(|event| event.sequence == sequence) {
            self.pop_head();
        }
    }

    /// A `reliable` subscriber acknowledged everything up to `sequence`.
    pub(crate) fn ack(&mut self, sequence: u64) {
        if self.qos != Qos::Reliable {
            return;
        }
        while self.peek().is_some_and(|front| front.sequence <= sequence) {
            self.pop_head();
        }
    }

    /// Every event the queues hold, for the ledger audit.
    #[cfg(test)]
    pub(crate) fn held_events(&self) -> impl Iterator<Item = &Event> {
        self.queue
            .iter()
            .chain(self.conflated.iter().map(|(_, event)| event))
    }

    /// Remove the head event; the bytes it held, or 0 when empty.
    fn pop_head(&mut self) -> usize {
        let event = match self.qos {
            Qos::Conflate if !self.conflated.is_empty() => Some(self.conflated.remove(0).1),
            Qos::Conflate => None,
            _ => self.queue.pop_front(),
        };
        let Some(event) = event else {
            return 0;
        };
        let cost = event_cost(&event);
        self.bytes = take_bytes(self.bytes, cost);
        cost
    }

    /// Queue one event under the subscription's QoS policy. `room` is how
    /// many more bytes the owner's budget has before this call; the event is
    /// admitted if it fits once the policy's own evictions are counted.
    ///
    /// * `latest`: one slot; a new event replaces the old one (a drop).
    /// * `buffered`: `depth` slots; overflow drops the oldest.
    /// * `conflate`: the latest event per publisher; replacing a publisher's
    ///   pending event drops the coalesced one. The window is "until
    ///   consumed": without a clock the broker cannot time-window, and the
    ///   subscriber's `NextEvent` is the natural boundary.
    /// * `reliable`: like buffered, but delivery does not pop; the head is
    ///   handed out until the subscriber acks it.
    pub(crate) fn push(&mut self, event: Event, room: usize) {
        match self.qos {
            Qos::Latest => self.push_bounded(event, 1, room),
            // Clamped again here: a subscription's depth bounds broker memory.
            Qos::Buffered(_) => self.push_bounded(event, self.qos.depth() as usize, room),
            Qos::Reliable => self.push_bounded(event, Qos::RELIABLE_DEPTH as usize, room),
            Qos::Conflate => self.push_conflated(event, room),
        }
    }

    fn push_bounded(&mut self, event: Event, depth: usize, mut room: usize) {
        while self.queue.len() >= depth.max(1) {
            room = room.saturating_add(self.pop_head());
            self.drops += 1;
        }
        if self.make_room(event_cost(&event), &mut room) {
            self.bytes += event_cost(&event);
            self.queue.push_back(event);
        } else {
            self.drops += 1;
        }
    }

    fn push_conflated(&mut self, event: Event, mut room: usize) {
        let cost = event_cost(&event);
        let same = self
            .conflated
            .iter()
            .position(|(publisher, _)| *publisher == event.publisher);
        if let Some(index) = same {
            let old = event_cost(&self.conflated[index].1);
            self.drops += 1;
            // The replacement's net change is what must fit.
            if cost <= room.saturating_add(old) {
                self.conflated[index] = (event.publisher, event);
                self.bytes = take_bytes(self.bytes, old) + cost;
                return;
            }
            self.conflated.remove(index);
            self.bytes = take_bytes(self.bytes, old);
            room = room.saturating_add(old);
        }
        let window = (Qos::CONFLATE_WINDOW as usize).max(1);
        while self.conflated.len() >= window {
            room = room.saturating_add(self.pop_head());
            self.drops += 1;
        }
        if self.make_room(cost, &mut room) {
            self.bytes += cost;
            self.conflated.push((event.publisher, event));
        } else {
            self.drops += 1;
        }
    }

    /// Shed the oldest events until `cost` fits `room`; `false` when even an
    /// empty queue has no room for it.
    fn make_room(&mut self, cost: usize, room: &mut usize) -> bool {
        while cost > *room {
            if self.queued() == 0 {
                return false;
            }
            *room = room.saturating_add(self.pop_head());
            self.drops += 1;
        }
        true
    }
}
