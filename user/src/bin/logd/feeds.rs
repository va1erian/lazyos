//! The topics `logd` records: `init`'s and the central broker's
//! `system/events/#`, `healthd`'s `system/health/+`, plus two sampled
//! counters (fabric denials and the central queue's drops).

use alloc::format;

use user::central;
use user::messenger::{self, router, services, topics_client};
use user::sys;

use crate::log::Log;
use crate::payload;

/// How often the fabric audit counters are sampled for denial records.
const DENIAL_POLL_TICKS: u64 = 25;
/// Queue depth for the central `system/events/#` audit feed. `Latest` (depth
/// one) would let the broker silently overwrite an event that arrives before
/// this loop's next poll; buffering gives the drain loop real headroom, with
/// any overflow still counted and logged (see [`Feeds::poll_overflow`])
/// rather than silently lost.
const CENTRAL_QUEUE_DEPTH: u32 = topics_client::Qos::MAX_DEPTH;
/// How often the central subscription's drop counter is sampled.
const OVERFLOW_POLL_TICKS: u64 = 25;

/// Every feed: a cached bus endpoint plus the attached sink, so a failed
/// subscribe retries without resolving a new handle every loop.
pub(super) struct Feeds {
    events_bus: Option<router::Bus>,
    events: Option<router::Subscriber>,
    health_bus: Option<router::Bus>,
    health: Option<router::Subscriber>,
    central: Option<central::Bus>,
    central_events: Option<central::Subscription>,
    central_warned: bool,
    central_drops: u64,
    audit: Option<(u64, u64, u64)>,
    next_denial_poll: u64,
    next_overflow_poll: u64,
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
            central_warned: false,
            central_drops: 0,
            audit: None,
            next_denial_poll: 0,
            next_overflow_poll: 0,
            stats_buffer: alloc::vec![0u8; messenger::FabricStats::SIZE],
            central_stats_buffer: alloc::vec![0u8; messenger::DEFAULT_BUFFER],
        }
    }

    /// Subscribe to whatever feed is not attached yet.
    pub(super) fn connect(&mut self) {
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
        if self.central_events.is_none() {
            self.connect_central();
        }
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
        match bus.subscribe_with_qos(
            "system/events/#",
            topics_client::Qos::Buffered(CENTRAL_QUEUE_DEPTH),
        ) {
            Ok(subscription) => {
                self.central_events = Some(subscription);
                self.central_drops = 0;
            }
            Err(error) => {
                // The bus itself may be the reason the subscribe failed (e.g.
                // the broker restarted); drop it too so the next loop resolves
                // a fresh one instead of retrying a dead handle forever.
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

    /// Move every queued `init`/`healthd` event into the log.
    pub(super) fn drain_local(&mut self, log: &mut Log, buffer: &mut [u8]) {
        drain(log, &self.events, buffer);
        drain(log, &self.health, buffer);
    }

    /// Move every queued central-broker event into the log. The central
    /// subscription hands back the same [`router::Event`] shape as the local
    /// one, so the record format is identical.
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
                    // The feed died (e.g. the broker restarted): drop both the
                    // subscription and the bus, or `connect`'s gate would
                    // never fire again and this dead handle would be retried
                    // forever.
                    self.central_events = None;
                    self.central = None;
                    return;
                }
            }
        }
    }

    /// Sample the counters that are due.
    pub(super) fn poll(&mut self, log: &mut Log, now: u64) {
        if now >= self.next_denial_poll {
            self.poll_denials(log);
            self.next_denial_poll = now + DENIAL_POLL_TICKS;
        }
        if now >= self.next_overflow_poll {
            self.poll_overflow(log);
            self.next_overflow_poll = now + OVERFLOW_POLL_TICKS;
        }
    }

    /// Append an overflow record when the central subscription's drop counter
    /// advances, so a `Buffered`-QoS queue that still overran (a burst larger
    /// than [`CENTRAL_QUEUE_DEPTH`]) leaves its own trace in the log.
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
    fn poll_denials(&mut self, log: &mut Log) {
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

/// Move every queued event from one subscriber into the log.
fn drain(log: &mut Log, subscriber: &Option<router::Subscriber>, buffer: &mut [u8]) {
    let Some(subscriber) = subscriber else {
        return;
    };
    loop {
        match subscriber.recv_with(buffer, Some(messenger::EXPIRED_DEADLINE)) {
            Ok(Some(event)) => log.append(
                &event.topic,
                &payload::describe(&event.topic, &event.payload),
            ),
            Ok(None) => return,
            // A feed error (e.g. the broker restarted) is retried next loop.
            Err(_) => return,
        }
    }
}
