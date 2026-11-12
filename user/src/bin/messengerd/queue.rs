//! One subscription's QoS queue and its byte accounting. Every push and pop
//! goes through here so [`Subscription::bytes`] is always what the queues
//! hold, which the broker sums against its memory budget.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use user::messenger::topics_client;

use super::filter::Filter;

/// What one queued event costs the broker: its payload, its topic and a flat
/// allowance for the record itself.
pub(super) fn event_cost(event: &topics_client::Event) -> usize {
    event.payload.len() + event.topic.len() + 64
}

/// One live subscription with its QoS queue and counters.
pub(super) struct Subscription {
    pub(super) id: u64,
    /// Task slot the subscribing call came from (kernel-stamped).
    pub(super) owner: u64,
    pub(super) filter: Filter,
    pub(super) qos: topics_client::Qos,
    /// Events ready for delivery (latest / buffered / reliable).
    queue: VecDeque<topics_client::Event>,
    /// Coalesced per-publisher slots (conflate only).
    conflated: Vec<(u64, topics_client::Event)>,
    /// Bytes the two queues hold ([`event_cost`] of each event).
    pub(super) bytes: usize,
    pub(super) drops: u64,
    pub(super) delivered: u64,
    pub(super) matched: u64,
    /// The owner's doorbell (`Bell`, P7.2): this end of its channel.
    pub(super) bell: Option<user::messenger::Endpoint>,
    /// Whether the bell rang since the owner last found the queue empty.
    pub(super) rung: bool,
}

impl Subscription {
    pub(super) fn new(id: u64, owner: u64, filter: Filter, qos: topics_client::Qos) -> Self {
        Subscription {
            id,
            owner,
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
    pub(super) fn queued(&self) -> u64 {
        (self.queue.len() + self.conflated.len()) as u64
    }

    /// The next event, without consuming it: the pop happens in
    /// [`Subscription::retire`] once the reply carrying the event has
    /// reached the subscriber. Borrowed, so a poll does not copy a payload.
    pub(super) fn peek(&self) -> Option<&topics_client::Event> {
        match self.qos {
            topics_client::Qos::Conflate => self.conflated.first().map(|(_, event)| event),
            _ => self.queue.front(),
        }
    }

    /// Pop the head when it is event `sequence` (a late commit cannot pop a
    /// newer event).
    pub(super) fn retire(&mut self, sequence: u64) {
        if self.peek().is_some_and(|event| event.sequence == sequence) {
            self.pop_head();
        }
    }

    /// A `reliable` subscriber acknowledged everything up to `sequence`.
    pub(super) fn ack(&mut self, sequence: u64) {
        if self.qos != topics_client::Qos::Reliable {
            return;
        }
        while self.peek().is_some_and(|front| front.sequence <= sequence) {
            self.pop_head();
        }
    }

    fn pop_head(&mut self) {
        let event = match self.qos {
            topics_client::Qos::Conflate if !self.conflated.is_empty() => {
                Some(self.conflated.remove(0).1)
            }
            topics_client::Qos::Conflate => None,
            _ => self.queue.pop_front(),
        };
        if let Some(event) = event {
            self.bytes = self.bytes.saturating_sub(event_cost(&event));
        }
    }

    fn push_back(&mut self, event: topics_client::Event) {
        self.bytes += event_cost(&event);
        self.queue.push_back(event);
    }

    /// Queue one event under the subscription's QoS policy.
    ///
    /// * `latest`: one slot; a new event replaces the old one (a drop).
    /// * `buffered`: `depth` slots; overflow drops the oldest.
    /// * `conflate`: the latest event per publisher; replacing a publisher's
    ///   pending event drops the coalesced one. The window is "until
    ///   consumed": without a clock the broker cannot time-window, and the
    ///   subscriber's `NextEvent` is the natural boundary.
    /// * `reliable`: like buffered, but delivery does not pop; the head is
    ///   handed out until the subscriber acks it.
    pub(super) fn push(&mut self, event: topics_client::Event) {
        match self.qos {
            topics_client::Qos::Latest => self.push_bounded(event, 1),
            // Clamped again here: a subscription's depth bounds broker memory.
            topics_client::Qos::Buffered(_) => self.push_bounded(event, self.qos.depth() as usize),
            topics_client::Qos::Reliable => {
                self.push_bounded(event, topics_client::Qos::RELIABLE_DEPTH as usize)
            }
            topics_client::Qos::Conflate => self.push_conflated(event),
        }
    }

    fn push_bounded(&mut self, event: topics_client::Event, depth: usize) {
        while self.queue.len() >= depth.max(1) {
            self.pop_head();
            self.drops += 1;
        }
        self.push_back(event);
    }

    fn push_conflated(&mut self, event: topics_client::Event) {
        if let Some(index) = self
            .conflated
            .iter()
            .position(|(publisher, _)| *publisher == event.publisher)
        {
            let old = core::mem::replace(&mut self.conflated[index], (event.publisher, event));
            self.bytes = self.bytes.saturating_sub(event_cost(&old.1));
            self.bytes += event_cost(&self.conflated[index].1);
            self.drops += 1;
            return;
        }
        let window = (topics_client::Qos::CONFLATE_WINDOW as usize).max(1);
        while self.conflated.len() >= window {
            self.pop_head();
            self.drops += 1;
        }
        self.bytes += event_cost(&event);
        self.conflated.push((event.publisher, event));
    }
}
