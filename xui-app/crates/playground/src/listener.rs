//! The listener's model: a ring of decoded events, the per-second cap, pause,
//! filter and pin, publisher names and the counters the stats strip shows
//! (`docs/messenger-playground-plan.md` D5, section 3).
//!
//! No I/O happens here. The window implements [`EventSource`] over the broker
//! (`Subscribe`, `NextEvent`, `Stats`, `Ack`) and its doorbell, and calls
//! [`Listener::drain`] when the bell rings: the drain pulls at most
//! `drain_batch` events a call, so a firehose cannot starve the UI thread,
//! and every pulled event is counted as stored, capped by the viewer, or
//! missed while paused, never silently lost.

use std::collections::VecDeque;

pub use crate::listener_decode::{Body, MAX_DECODE, MAX_KEPT};
pub use crate::listener_event::{Event, Registry, MAX_NAME_AGE_MS};

/// Events the ring keeps before the oldest is dropped.
pub const DEFAULT_RING: usize = 1000;
/// Events stored per second; the rest are counted, not kept.
pub const DEFAULT_MAX_PER_SEC: u32 = 200;
/// Events one drain pulls at most.
pub const DEFAULT_DRAIN_BATCH: usize = 256;
/// Pinned events kept at most.
pub const MAX_PINNED: usize = 64;

/// The broker's `Event`, as `NextEvent` returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEvent {
    pub topic: String,
    pub publisher: u64,
    pub sequence: u64,
    pub retained: bool,
    /// The payload as carried (inside the platform envelope).
    pub payload: Vec<u8>,
}

/// The broker's `Stats` of a subscription.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BrokerStats {
    pub qos: u32,
    pub depth: u32,
    pub queued: u64,
    pub delivered: u64,
    pub matched: u64,
    pub drops: u64,
}

/// The broker, as the listener needs it.
pub trait EventSource {
    type Error: std::fmt::Display;

    /// `Subscribe`: returns the subscription id.
    fn subscribe(&mut self, filter: &str, qos: u32, depth: u32) -> Result<u64, Self::Error>;
    /// `NextEvent` with an expired deadline: an event if one is ready.
    fn next_event(&mut self, subscription: u64) -> Result<Option<RawEvent>, Self::Error>;
    fn stats(&mut self, subscription: u64) -> Result<BrokerStats, Self::Error>;
    /// `Ack`, for a reliable subscription.
    fn ack(&mut self, subscription: u64, sequence: u64) -> Result<(), Self::Error>;
    fn unsubscribe(&mut self, subscription: u64) -> Result<(), Self::Error>;
}

/// How a subscription is made and how fast its events are kept.
#[derive(Debug, Clone)]
pub struct Config {
    pub filter: String,
    /// The broker's `Qos` index.
    pub qos: u32,
    pub depth: u32,
    pub ring: usize,
    pub max_per_sec: u32,
    pub drain_batch: usize,
}

impl Config {
    pub fn new(filter: &str) -> Config {
        Config {
            filter: filter.to_string(),
            qos: 1,
            depth: 16,
            ring: DEFAULT_RING,
            max_per_sec: DEFAULT_MAX_PER_SEC,
            drain_batch: DEFAULT_DRAIN_BATCH,
        }
    }
}

/// What happened to a pushed event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    Stored,
    /// Over the per-second cap: counted, not kept.
    RateCapped,
    /// The view is paused: counted, not kept.
    Paused,
}

/// One drain call's result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drain {
    pub pulled: usize,
    /// The batch limit stopped the drain: call again soon.
    pub more: bool,
}

/// Everything the strip under the list shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsStrip {
    pub qos: &'static str,
    pub depth: u32,
    pub queued: u64,
    pub delivered: u64,
    pub matched: u64,
    /// The broker's own drops (the QoS policy discarding events).
    pub drops: u64,
    /// Events the viewer dropped itself (over the per-second cap).
    pub dropped_by_viewer: u64,
    pub missed_while_paused: u64,
    pub shown: usize,
    pub stored: usize,
    pub pinned: usize,
    pub paused: bool,
}

/// A subscription's view.
pub struct Listener {
    config: Config,
    subscription: Option<u64>,
    ring: VecDeque<Event>,
    pinned: Vec<Event>,
    filter: String,
    paused: bool,
    next_id: u64,
    window_sec: u64,
    window_count: u32,
    received: u64,
    stored_total: u64,
    dropped_by_viewer: u64,
    missed_while_paused: u64,
    evicted: u64,
    cleared: u64,
}

impl Listener {
    /// A model with no subscription (tests, a replay).
    pub fn offline(config: Config) -> Listener {
        Listener {
            config,
            subscription: None,
            ring: VecDeque::new(),
            pinned: Vec::new(),
            filter: String::new(),
            paused: false,
            next_id: 1,
            window_sec: u64::MAX,
            window_count: 0,
            received: 0,
            stored_total: 0,
            dropped_by_viewer: 0,
            missed_while_paused: 0,
            evicted: 0,
            cleared: 0,
        }
    }

    /// Subscribe through `source`.
    pub fn start<S: EventSource>(source: &mut S, config: Config) -> Result<Listener, S::Error> {
        let id = source.subscribe(&config.filter, config.qos, config.depth)?;
        let mut listener = Listener::offline(config);
        listener.subscription = Some(id);
        Ok(listener)
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The broker's id of the subscription, while it is live.
    pub fn subscription(&self) -> Option<u64> {
        self.subscription
    }

    /// Unsubscribe; the events stay readable.
    pub fn stop<S: EventSource>(&mut self, source: &mut S) -> Result<(), S::Error> {
        match self.subscription.take() {
            Some(id) => source.unsubscribe(id),
            None => Ok(()),
        }
    }

    /// Pull ready events from the broker, up to the batch size. Reliable
    /// subscriptions are acked up to the last event pulled, kept or not.
    pub fn drain<S: EventSource>(
        &mut self,
        source: &mut S,
        now_ms: u64,
        registry: &Registry,
    ) -> Result<Drain, S::Error> {
        let Some(id) = self.subscription else {
            return Ok(Drain {
                pulled: 0,
                more: false,
            });
        };
        let mut pulled = 0;
        let mut last = None;
        let mut failure = None;
        while pulled < self.config.drain_batch {
            match source.next_event(id) {
                Ok(Some(raw)) => {
                    last = Some(raw.sequence);
                    pulled += 1;
                    self.push(now_ms, raw, registry);
                }
                Ok(None) => break,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if let (3, Some(sequence)) = (self.config.qos, last) {
            source.ack(id, sequence)?;
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(Drain {
            pulled,
            more: pulled == self.config.drain_batch,
        })
    }

    /// Account for one event pulled at `now_ms`.
    pub fn push(&mut self, now_ms: u64, raw: RawEvent, registry: &Registry) -> Admit {
        self.received += 1;
        if self.paused {
            self.missed_while_paused += 1;
            return Admit::Paused;
        }
        let second = now_ms / 1000;
        if second != self.window_sec {
            self.window_sec = second;
            self.window_count = 0;
        }
        if self.window_count >= self.config.max_per_sec {
            self.dropped_by_viewer += 1;
            return Admit::RateCapped;
        }
        self.window_count += 1;
        let name = registry.resolve(raw.publisher, now_ms).map(str::to_string);
        let event = Event::new(self.next_id, now_ms, raw, name);
        self.next_id += 1;
        self.stored_total += 1;
        while self.ring.len() >= self.config.ring.max(1) {
            self.ring.pop_front();
            self.evicted += 1;
        }
        self.ring.push_back(event);
        Admit::Stored
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn set_filter(&mut self, text: &str) {
        self.filter = text.to_lowercase();
    }

    /// Empty the list (pinned events stay, counters keep counting).
    pub fn clear(&mut self) {
        self.cleared += self.ring.len() as u64;
        self.ring.clear();
    }

    /// The events matching the filter, oldest first.
    pub fn visible(&self) -> impl Iterator<Item = &Event> {
        let needle = self.filter.clone();
        self.ring
            .iter()
            .filter(move |e| needle.is_empty() || e.search.contains(&needle))
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Keep event `id` through eviction and Clear. False when it is gone or
    /// the pin list is full.
    pub fn pin(&mut self, id: u64) -> bool {
        if self.pinned.len() >= MAX_PINNED || self.pinned.iter().any(|e| e.id == id) {
            return false;
        }
        match self.ring.iter().find(|e| e.id == id) {
            Some(event) => {
                self.pinned.push(event.clone());
                true
            }
            None => false,
        }
    }

    pub fn unpin(&mut self, id: u64) {
        self.pinned.retain(|e| e.id != id);
    }

    pub fn pinned(&self) -> &[Event] {
        &self.pinned
    }

    pub fn is_pinned(&self, id: u64) -> bool {
        self.pinned.iter().any(|e| e.id == id)
    }

    /// Events pulled from the broker, kept or not.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Events dropped by the ring being full.
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// The strip's numbers; `broker` is the latest `Stats` read.
    pub fn strip(&self, broker: Option<BrokerStats>) -> StatsStrip {
        let b = broker.unwrap_or(BrokerStats {
            qos: self.config.qos,
            depth: self.config.depth,
            ..BrokerStats::default()
        });
        StatsStrip {
            qos: rhai_lazy::msg::topics::QOS[b.qos as usize % 4],
            depth: b.depth,
            queued: b.queued,
            delivered: b.delivered,
            matched: b.matched,
            drops: b.drops,
            dropped_by_viewer: self.dropped_by_viewer,
            missed_while_paused: self.missed_while_paused,
            shown: self.visible().count(),
            stored: self.ring.len(),
            pinned: self.pinned.len(),
            paused: self.paused,
        }
    }

    /// Every pulled event is exactly one of: stored (now or since evicted),
    /// capped by the viewer, or missed while paused.
    pub fn accounted(&self) -> bool {
        self.received == self.stored_total + self.dropped_by_viewer + self.missed_while_paused
            && self.stored_total == self.ring.len() as u64 + self.evicted + self.cleared
    }
}

/// Read what a topic holds right now: subscribe, take the events that are
/// ready (a retained value replays at once), unsubscribe. At most `max`
/// events; the subscription is closed even when a read fails.
pub fn peek<S: EventSource>(
    source: &mut S,
    filter: &str,
    max: usize,
    now_ms: u64,
    registry: &Registry,
) -> Result<Vec<Event>, S::Error> {
    let mut config = Config::new(filter);
    config.qos = 1;
    config.depth = max.clamp(1, 64) as u32;
    config.drain_batch = max.max(1);
    config.max_per_sec = u32::MAX;
    let mut listener = Listener::start(source, config)?;
    let drained = listener.drain(source, now_ms, registry);
    let stopped = listener.stop(source);
    drained?;
    stopped?;
    Ok(listener.ring.into_iter().collect())
}
