//! The topics `logd` records: the central broker's `system/events/#` (the
//! services' state events, login, package and clipboard records, ...) and
//! `system/health/+` (`healthd`'s rows), plus two sampled counters (fabric
//! denials and the central queues' drops).
//!
//! Every feed has a wake source `logd` parks on (P7.2): the central
//! subscriptions ring their doorbell (`topics.Bell`) when events are waiting.
//! The denial counters have none (the kernel's audit ring is written under
//! locks a wake may not take), so they are sampled on every wake and on a slow
//! fallback timer.

use alloc::format;
use alloc::string::String;

use user::central;
use user::messenger::{self, services, topics_client, Endpoint};
use user::sys;

use crate::log::Log;
use crate::payload;

/// How often the fabric audit counters are sampled when nothing else wakes
/// `logd` (it samples them on every wake too).
pub(super) const DENIAL_FALLBACK_TICKS: u64 = 1000;
/// How soon to try again while a feed cannot be attached (its broker is not
/// up yet).
pub(super) const CONNECT_RETRY_TICKS: u64 = 10;
/// Queue depth for the central feeds. `Latest` (depth one) would let the
/// broker silently overwrite an event that arrives before this loop drains
/// it; buffering gives the drain real headroom, with any overflow still
/// counted and logged (see [`Feed::poll_overflow`]) rather than silently
/// lost.
const CENTRAL_QUEUE_DEPTH: u32 = topics_client::Qos::MAX_DEPTH;
/// The platform's events: every `system/events/...` topic.
const EVENTS_FILTER: &str = "system/events/#";

/// One source of work [`Feeds::wait_set`] hands the loop.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    /// `system/events/#`.
    Events,
    /// `system/health/+`.
    Health,
}

/// One central subscription with its doorbell.
#[derive(Default)]
struct Feed {
    filter: String,
    subscription: Option<central::Subscription>,
    bell: Option<Endpoint>,
    drops: u64,
}

impl Feed {
    fn new(filter: String) -> Feed {
        Feed {
            filter,
            ..Feed::default()
        }
    }

    /// Forget a dead feed: its bell closes, and it is attached afresh.
    fn drop_dead(&mut self) {
        self.subscription = None;
        if let Some(bell) = self.bell.take() {
            let _ = bell.close();
        }
    }

    /// Subscribe with a queue deep enough to drain at leisure, and attach the
    /// doorbell.
    fn attach(&mut self, bus: &mut central::Bus) -> messenger::Result<()> {
        let mut subscription = bus.subscribe_with_qos(
            &self.filter,
            topics_client::Qos::Buffered(CENTRAL_QUEUE_DEPTH),
        )?;
        self.bell = Some(subscription.bell()?);
        self.subscription = Some(subscription);
        self.drops = 0;
        Ok(())
    }

    /// Move every queued event into the log; the empty pull at the end
    /// re-arms the doorbell. `false` when the feed died.
    fn drain(&mut self, log: &mut Log, buffer: &mut [u8]) -> bool {
        let Some(sub) = self.subscription.as_ref() else {
            return true;
        };
        loop {
            match sub.recv_with(buffer, Some(messenger::EXPIRED_DEADLINE)) {
                Ok(Some(event)) => log.append(
                    &event.topic,
                    &payload::describe(&event.topic, &event.payload),
                ),
                Ok(None) => return true,
                Err(_) => {
                    self.drop_dead();
                    return false;
                }
            }
        }
    }

    /// Append an overflow record when the drop counter advances, so a
    /// `Buffered` queue that still overran (a burst larger than
    /// [`CENTRAL_QUEUE_DEPTH`]) leaves its own trace in the log. Drops only
    /// happen when events arrive, which rings the bell: checked there.
    fn poll_overflow(&mut self, log: &mut Log, buffer: &mut [u8]) {
        let Some(subscriber) = &self.subscription else {
            return;
        };
        let Ok(stats) = subscriber.stats_with(buffer) else {
            return;
        };
        if stats.drops <= self.drops {
            return;
        }
        let delta = stats.drops - self.drops;
        self.drops = stats.drops;
        let detail = format!(
            "filter={} dropped={delta} total_drops={} qos={} depth={} queued={}",
            self.filter, stats.drops, stats.qos, stats.depth, stats.queued
        );
        log.append(logstore::source::OVERFLOW_TOPIC, &detail);
    }
}

/// Every feed: a cached bus endpoint plus the attached subscriptions, so a
/// failed subscribe retries without resolving a new handle every loop.
pub(super) struct Feeds {
    central: Option<central::Bus>,
    events: Feed,
    health: Feed,
    central_warned: bool,
    audit: Option<(u64, u64, u64)>,
    // Reused snapshot buffers: the user heap never reclaims large per-call
    // buffers.
    stats_buffer: alloc::vec::Vec<u8>,
    central_stats_buffer: alloc::vec::Vec<u8>,
}

impl Feeds {
    pub(super) fn new() -> Feeds {
        // The declared `system/health/{name}` pattern with its `+` wildcard;
        // it also matches the literal `summary` topic.
        let health = services::health::wire::filter_system_health("+").unwrap_or_default();
        Feeds {
            central: None,
            events: Feed::new(String::from(EVENTS_FILTER)),
            health: Feed::new(health),
            central_warned: false,
            audit: None,
            stats_buffer: alloc::vec![0u8; messenger::FabricStats::SIZE],
            central_stats_buffer: alloc::vec![0u8; messenger::DEFAULT_BUFFER],
        }
    }

    /// Subscribe to whatever feed is not attached yet; `true` when all are.
    pub(super) fn connect(&mut self) -> bool {
        if self.events.bell.is_some() && self.health.bell.is_some() {
            return true;
        }
        if self.central.is_none() {
            match central::Bus::connect() {
                Ok(bus) => self.central = Some(bus),
                Err(error) => {
                    self.warn("logd: central broker unavailable: ", error.message());
                    return false;
                }
            }
        }
        for feed in [&mut self.events, &mut self.health] {
            let Some(bus) = &mut self.central else {
                break;
            };
            if feed.bell.is_some() || feed.filter.is_empty() {
                continue;
            }
            if let Err(error) = feed.attach(bus) {
                // The bus itself may be the reason the subscribe failed (e.g.
                // the broker restarted); drop it too so the next attempt
                // resolves a fresh one instead of retrying a dead handle.
                self.central = None;
                if !self.central_warned {
                    self.central_warned = true;
                    sys::write_str("logd: central subscribe failed: ");
                    sys::write_str(error.message());
                    sys::write_str("\n");
                }
                break;
            }
        }
        self.events.bell.is_some() && self.health.bell.is_some()
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
    /// `sources` (at least two slots each); returns how many.
    pub(super) fn wait_set(&self, ends: &mut [Endpoint], sources: &mut [Source]) -> usize {
        let mut count = 0;
        for (end, source) in [
            (self.events.bell, Source::Events),
            (self.health.bell, Source::Health),
        ] {
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
        let feed = match source {
            Source::Events => &mut self.events,
            Source::Health => &mut self.health,
        };
        if let Some(sub) = &feed.subscription {
            sub.take_ring(buffer);
        }
        if feed.drain(log, buffer) {
            feed.poll_overflow(log, &mut self.central_stats_buffer);
        } else {
            // The broker restarted: attach afresh through a new bus.
            self.central = None;
        }
    }

    /// Move every queued event into the log (the shutdown's last look).
    pub(super) fn drain_all(&mut self, log: &mut Log, buffer: &mut [u8]) {
        self.events.drain(log, buffer);
        self.health.drain(log, buffer);
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
