//! The topics `logd` records: `init`'s and the central broker's
//! `system/events/#`, `healthd`'s `system/health/+`, plus two sampled
//! counters (fabric denials and the central queue's drops).
//!
//! Every feed has a wake source `logd` parks on (P7.2): the two local brokers
//! push into endpoints of ours, and the central subscription rings its
//! doorbell (`topics.Bell`) when events are waiting. The denial counters have
//! none (the kernel's audit ring is written under locks a wake may not take),
//! so they are sampled on every wake and on a slow fallback timer.

use alloc::format;

use user::central;
use user::messenger::{self, router, services, topics_client, Endpoint};
use user::sys;

use crate::log::Log;
use crate::payload;

/// How often the fabric audit counters are sampled when nothing else wakes
/// `logd` (it samples them on every wake too).
pub(super) const DENIAL_FALLBACK_TICKS: u64 = 1000;
/// How soon to try again while a feed cannot be attached (its broker is not
/// up yet: `healthd` starts after `logd`).
pub(super) const CONNECT_RETRY_TICKS: u64 = 10;
/// Queue depth for the central `system/events/#` audit feed. `Latest` (depth
/// one) would let the broker silently overwrite an event that arrives before
/// this loop drains it; buffering gives the drain real headroom, with any
/// overflow still counted and logged (see [`Feeds::poll_overflow`]) rather
/// than silently lost.
const CENTRAL_QUEUE_DEPTH: u32 = topics_client::Qos::MAX_DEPTH;

/// One source of work [`Feeds::wait_set`] hands the loop.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Events,
    Health,
    Central,
}

/// Every feed: a cached bus endpoint plus the attached sink, so a failed
/// subscribe retries without resolving a new handle every loop.
pub(super) struct Feeds {
    events_bus: Option<router::Bus>,
    events: Option<router::Subscriber>,
    health_bus: Option<router::Bus>,
    health: Option<router::Subscriber>,
    central: Option<central::Bus>,
    central_events: Option<central::Subscription>,
    /// The central subscription's doorbell.
    central_bell: Option<Endpoint>,
    central_warned: bool,
    central_drops: u64,
    audit: Option<(u64, u64, u64)>,
    // Reused snapshot buffers: the user heap never reclaims large per-call
    // buffers.
    stats_buffer: alloc::vec::Vec<u8>,
    central_stats_buffer: alloc::vec::Vec<u8>,
}

impl Feeds {
    pub(super) fn new() -> Feeds {
        Feeds {
            events_bus: None,
            events: None,
            health_bus: None,
            health: None,
            central: None,
            central_events: None,
            central_bell: None,
            central_warned: false,
            central_drops: 0,
            audit: None,
            stats_buffer: alloc::vec![0u8; messenger::FabricStats::SIZE],
            central_stats_buffer: alloc::vec![0u8; messenger::DEFAULT_BUFFER],
        }
    }

    /// Subscribe to whatever feed is not attached yet; `true` when all are.
    pub(super) fn connect(&mut self) -> bool {
        if self.events.is_none() {
            self.events_bus = connect_or_keep(self.events_bus.take(), services::INIT_NAME);
            if let Some(bus) = &self.events_bus {
                self.events = bus.subscribe("system/events/#").ok();
            }
        }
        if self.health.is_none() {
            self.health_bus = connect_or_keep(self.health_bus.take(), services::HEALTHD_NAME);
            if let Some(bus) = &mut self.health_bus {
                // The declared `system/health/{name}` pattern with its `+`
                // wildcard; it also matches the literal `summary` topic.
                self.health = services::health::wire::subscribe_system_health(bus, "+").ok();
            }
        }
        // Centrally published service events (`mimed` launch records, the
        // clipboard audit trail). The central broker isn't batch-subscribed
        // by the router, so this is a separate client and sink.
        if self.central_bell.is_none() {
            self.connect_central();
        }
        self.events.is_some() && self.health.is_some() && self.central_bell.is_some()
    }

    fn connect_central(&mut self) {
        if self.central.is_none() {
            match central::Bus::connect() {
                Ok(bus) => self.central = Some(bus),
                Err(error) => self.warn("logd: central broker unavailable: ", error.message()),
            }
        }
        let Some(bus) = &mut self.central else {
            return;
        };
        let subscribed = bus
            .subscribe_with_qos(
                "system/events/#",
                topics_client::Qos::Buffered(CENTRAL_QUEUE_DEPTH),
            )
            .and_then(|mut subscription| {
                let bell = subscription.bell()?;
                Ok((subscription, bell))
            });
        match subscribed {
            Ok((subscription, bell)) => {
                self.central_events = Some(subscription);
                self.central_bell = Some(bell);
                self.central_drops = 0;
            }
            Err(error) => {
                // The bus itself may be the reason the subscribe failed (e.g.
                // the broker restarted); drop it too so the next attempt
                // resolves a fresh one instead of retrying a dead handle.
                self.central = None;
                self.warn("logd: central subscribe failed: ", error.message());
            }
        }
    }

    /// Print one central-broker warning per run.
    fn warn(&mut self, what: &str, message: &str) {
        if !self.central_warned {
            self.central_warned = true;
            sys::write_str(what);
            sys::write_str(message);
            sys::write_str("\n");
        }
    }

    /// The endpoints to park on beside the service's own, into `ends` and
    /// `sources` (at least three slots each); returns how many.
    pub(super) fn wait_set(&self, ends: &mut [Endpoint], sources: &mut [Source]) -> usize {
        let mut count = 0;
        let feeds = [
            (
                self.events.as_ref().map(router::Subscriber::endpoint),
                Source::Events,
            ),
            (
                self.health.as_ref().map(router::Subscriber::endpoint),
                Source::Health,
            ),
            (self.central_bell, Source::Central),
        ];
        for (end, source) in feeds {
            if let Some(end) = end {
                ends[count] = end;
                sources[count] = source;
                count += 1;
            }
        }
        count
    }

    /// Take what `source` signalled into the log.
    pub(super) fn take(&mut self, source: Source, log: &mut Log, buffer: &mut [u8]) {
        match source {
            Source::Events => {
                if !take_one(log, &self.events, buffer) {
                    self.events = None;
                }
            }
            Source::Health => {
                if !take_one(log, &self.health, buffer) {
                    self.health = None;
                }
            }
            Source::Central => {
                if let Some(sub) = &self.central_events {
                    sub.take_ring(buffer);
                }
                self.drain_central(log, buffer);
                self.poll_overflow(log);
            }
        }
    }

    /// Move every queued `init`/`healthd` event into the log (the shutdown's
    /// last look).
    pub(super) fn drain_local(&mut self, log: &mut Log, buffer: &mut [u8]) {
        drain(log, &self.events, buffer);
        drain(log, &self.health, buffer);
    }

    /// Move every queued central-broker event into the log; the empty pull at
    /// the end re-arms the doorbell. The central subscription hands back the
    /// same [`router::Event`] shape as the local one, so the record format is
    /// identical.
    pub(super) fn drain_central(&mut self, log: &mut Log, buffer: &mut [u8]) {
        let Some(sub) = self.central_events.as_ref() else {
            return;
        };
        loop {
            match sub.recv_with(buffer, Some(messenger::EXPIRED_DEADLINE)) {
                Ok(Some(event)) => log.append(
                    &event.topic,
                    &payload::describe(&event.topic, &event.payload),
                ),
                Ok(None) => return,
                Err(_) => {
                    // The feed died (e.g. the broker restarted): drop the
                    // subscription, its bell and the bus, so `connect`
                    // attaches afresh instead of retrying a dead handle.
                    self.central_events = None;
                    if let Some(bell) = self.central_bell.take() {
                        let _ = bell.close();
                    }
                    self.central = None;
                    return;
                }
            }
        }
    }

    /// Append an overflow record when the central subscription's drop counter
    /// advances, so a `Buffered`-QoS queue that still overran (a burst larger
    /// than [`CENTRAL_QUEUE_DEPTH`]) leaves its own trace in the log. Drops
    /// only happen when events arrive, which rings the bell: checked there.
    fn poll_overflow(&mut self, log: &mut Log) {
        let Some(subscriber) = &self.central_events else {
            return;
        };
        let Ok(stats) = subscriber.stats_with(&mut self.central_stats_buffer) else {
            return;
        };
        if stats.drops <= self.central_drops {
            return;
        }
        let delta = stats.drops - self.central_drops;
        self.central_drops = stats.drops;
        let detail = format!(
            "dropped={delta} total_drops={} qos={} depth={} queued={}",
            stats.drops, stats.qos, stats.depth, stats.queued
        );
        log.append("system/events/audit/overflow", &detail);
    }

    /// Append a record when the fabric audit counters advance.
    pub(super) fn poll_denials(&mut self, log: &mut Log) {
        let Ok(stats) = messenger::fabric_stats_with(&mut self.stats_buffer) else {
            return;
        };
        let counter = (stats.audit_total, stats.audit_denies, stats.audit_last_hash);
        if self.audit.is_some_and(|previous| previous == counter) {
            return;
        }
        // The first sample only establishes a baseline unless denials already
        // happened; later samples record the delta of the fabric audit ring.
        if self.audit.is_some() || counter.0 > 0 {
            let detail = format!(
                "audit_total={} denies={} allows={} last_hash=0x{:016x}",
                stats.audit_total, stats.audit_denies, stats.audit_allows, stats.audit_last_hash
            );
            log.append(logstore::source::DENIAL_TOPIC, &detail);
        }
        self.audit = Some(counter);
    }
}

/// Reuse a cached bus, or connect once when the service appears.
fn connect_or_keep(bus: Option<router::Bus>, name: &str) -> Option<router::Bus> {
    match bus {
        Some(bus) => Some(bus),
        None => router::Bus::connect(name).ok(),
    }
}

/// Move one queued event from a subscriber into the log (the wait said one
/// is there); `false` when the feed failed (its broker is gone).
fn take_one(log: &mut Log, subscriber: &Option<router::Subscriber>, buffer: &mut [u8]) -> bool {
    let Some(subscriber) = subscriber else {
        return true;
    };
    match subscriber.recv_with(buffer, Some(messenger::EXPIRED_DEADLINE)) {
        Ok(Some(event)) => {
            log.append(
                &event.topic,
                &payload::describe(&event.topic, &event.payload),
            );
            true
        }
        Ok(None) => true,
        Err(_) => false,
    }
}

/// Move every queued event from one subscriber into the log.
fn drain(log: &mut Log, subscriber: &Option<router::Subscriber>, buffer: &mut [u8]) {
    while let Some(subscriber) = subscriber {
        match subscriber.recv_with(buffer, Some(messenger::EXPIRED_DEADLINE)) {
            Ok(Some(event)) => log.append(
                &event.topic,
                &payload::describe(&event.topic, &event.payload),
            ),
            _ => return,
        }
    }
}
