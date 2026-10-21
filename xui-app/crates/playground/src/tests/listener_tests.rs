use std::collections::VecDeque;

use rhai::{Dynamic, Map};
use rhai_lazy::msg::topics;

use super::{raw, tick_payload};
use crate::catalog::Catalog;
use crate::listener::{
    peek, Admit, BrokerStats, Config, Event, EventSource, Listener, RawEvent, Registry,
    MAX_NAME_AGE_MS, MAX_PINNED,
};
use crate::live::RegistryRow;
use crate::testkit::cases;

fn registry(rows: &[(&str, u64)], taken_ms: u64) -> Registry {
    let catalog = Catalog::from_schema();
    let rows: Vec<RegistryRow> = rows
        .iter()
        .map(|(name, owner)| RegistryRow {
            name: name.to_string(),
            owner: *owner,
            ..RegistryRow::default()
        })
        .collect();
    Registry::from_join(&catalog.join_live(&rows), taken_ms)
}

fn push(l: &mut Listener, now_ms: u64, seq: u64, registry: &Registry) -> Admit {
    l.push(
        now_ms,
        raw("time/tick", 5, seq, tick_payload(seq as i64)),
        registry,
    )
}

#[test]
fn events_are_kept_in_a_ring_and_the_oldest_go_first() {
    let mut config = Config::new("time/tick");
    config.ring = 3;
    config.max_per_sec = 1000;
    let mut l = Listener::offline(config);
    let reg = Registry::default();
    for i in 1..=5 {
        assert_eq!(push(&mut l, 1000 + i, i, &reg), Admit::Stored);
    }
    let seqs: Vec<u64> = l.visible().map(|e| e.sequence).collect();
    assert_eq!(seqs, [3, 4, 5]);
    assert_eq!(l.evicted(), 2);
    assert_eq!(l.received(), 5);
    assert!(l.accounted());
    l.clear();
    assert!(l.is_empty());
    assert!(l.accounted(), "clear keeps the counters honest");
}

#[test]
fn a_per_second_cap_counts_what_the_viewer_dropped() {
    let mut config = Config::new("#");
    config.max_per_sec = 3;
    let mut l = Listener::offline(config);
    let reg = Registry::default();
    let mut admits = Vec::new();
    for i in 0..5 {
        admits.push(push(&mut l, 5_000 + i, i, &reg));
    }
    assert_eq!(
        admits,
        [
            Admit::Stored,
            Admit::Stored,
            Admit::Stored,
            Admit::RateCapped,
            Admit::RateCapped
        ]
    );
    // The next second starts a new window.
    assert_eq!(push(&mut l, 6_000, 9, &reg), Admit::Stored);
    let strip = l.strip(None);
    assert_eq!(strip.dropped_by_viewer, 2);
    assert_eq!(strip.stored, 4);
    assert!(l.accounted());
}

#[test]
fn pause_counts_what_it_misses_and_resume_keeps_going() {
    let mut l = Listener::offline(Config::new("#"));
    let reg = Registry::default();
    push(&mut l, 1000, 1, &reg);
    l.pause();
    assert!(l.paused());
    assert_eq!(push(&mut l, 1001, 2, &reg), Admit::Paused);
    assert_eq!(push(&mut l, 1002, 3, &reg), Admit::Paused);
    l.resume();
    push(&mut l, 1003, 4, &reg);
    let strip = l.strip(None);
    assert_eq!(strip.missed_while_paused, 2);
    assert_eq!(l.visible().count(), 2);
    assert!(!strip.paused);
    assert!(l.accounted());
}

#[test]
fn the_filter_matches_topic_publisher_and_payload_text() {
    let mut l = Listener::offline(Config::new("#"));
    let reg = registry(&[("os.lazy.timed", 5)], 1000);
    l.push(1000, raw("time/tick", 5, 1, tick_payload(1)), &reg);
    l.push(
        1001,
        raw("demo/a", 6, 2, topics::wrap(b"hello world").unwrap()),
        &reg,
    );
    l.push(
        1002,
        raw("demo/b", 7, 3, topics::wrap(b"other").unwrap()),
        &reg,
    );
    let count = |l: &Listener| l.visible().count();
    l.set_filter("");
    assert_eq!(count(&l), 3);
    l.set_filter("DEMO/");
    assert_eq!(count(&l), 2);
    l.set_filter("europe");
    assert_eq!(count(&l), 1, "the payload text is searched");
    l.set_filter("os.lazy.timed");
    assert_eq!(count(&l), 1, "so is the publisher's name");
    l.set_filter("no such text");
    assert_eq!(count(&l), 0);
    assert_eq!(l.strip(None).shown, 0);
    assert_eq!(l.strip(None).stored, 3);
}

#[test]
fn pinned_events_survive_eviction_and_clear_up_to_a_limit() {
    let mut config = Config::new("#");
    config.ring = 2;
    config.max_per_sec = 10_000;
    let mut l = Listener::offline(config);
    let reg = Registry::default();
    push(&mut l, 1000, 1, &reg);
    let first = l.visible().next().unwrap().id;
    assert!(l.pin(first));
    assert!(!l.pin(first), "already pinned");
    assert!(!l.pin(9999), "no such event");
    for i in 2..6 {
        push(&mut l, 1000 + i, i, &reg);
    }
    assert!(l.visible().all(|e| e.id != first), "evicted from the ring");
    assert_eq!(l.pinned()[0].sequence, 1);
    l.clear();
    assert_eq!(l.pinned().len(), 1);
    assert!(l.is_pinned(first));
    l.unpin(first);
    assert!(l.pinned().is_empty());
    // The pin list is bounded.
    let mut many = Config::new("#");
    many.ring = 1000;
    many.max_per_sec = 100_000;
    let mut l = Listener::offline(many);
    for i in 0..(MAX_PINNED as u64 + 10) {
        push(&mut l, 1000, i, &reg);
    }
    let ids: Vec<u64> = l.visible().map(|e| e.id).collect();
    let pinned = ids.iter().filter(|id| l.pin(**id)).count();
    assert_eq!(pinned, MAX_PINNED);
}

#[test]
fn a_publisher_is_a_name_only_when_it_is_known_now() {
    let mut l = Listener::offline(Config::new("#"));
    let reg = registry(&[("os.lazy.timed", 5), ("dup.a", 8), ("dup.b", 8)], 10_000);
    l.push(10_100, raw("time/tick", 5, 1, tick_payload(1)), &reg);
    l.push(10_200, raw("time/tick", 99, 2, tick_payload(2)), &reg);
    l.push(10_300, raw("time/tick", 8, 3, tick_payload(3)), &reg);
    // A snapshot that is too old names nobody: slots are reused.
    l.push(
        10_000 + MAX_NAME_AGE_MS + 1,
        raw("time/tick", 5, 4, tick_payload(4)),
        &reg,
    );
    let names: Vec<String> = l.visible().map(Event::publisher_text).collect();
    assert_eq!(names, ["os.lazy.timed", "?", "?", "?"]);
    // The name an event got is the one it had when it arrived.
    let fresh = registry(&[("someone.else", 5)], 20_000);
    l.push(20_100, raw("time/tick", 5, 5, tick_payload(5)), &fresh);
    let last = l.visible().last().unwrap();
    assert_eq!(last.publisher_text(), "someone.else");
    assert_eq!(
        l.visible().next().unwrap().publisher_text(),
        "os.lazy.timed"
    );
}

#[test]
fn an_event_copies_as_the_map_a_script_would_see() {
    let mut l = Listener::offline(Config::new("#"));
    l.push(
        1000,
        raw("time/tick", 5, 7, tick_payload(9)),
        &Registry::default(),
    );
    let text = l.visible().next().unwrap().to_rhai();
    let engine = rhai::Engine::new();
    let map = engine.eval::<Dynamic>(&text).unwrap().cast::<Map>();
    assert_eq!(map["sequence"].as_int().unwrap(), 7);
    assert_eq!(map["topic"].clone().into_string().unwrap(), "time/tick");
    assert_eq!(
        map["payload"].clone().cast::<Map>()["unix"]
            .as_int()
            .unwrap(),
        9
    );
}

/// A broker stand-in: a bounded queue, fed by the test.
#[derive(Default)]
struct Sim {
    queue: VecDeque<RawEvent>,
    acks: Vec<u64>,
    subscribed: Vec<u64>,
    unsubscribed: Vec<u64>,
    fail_after: Option<usize>,
    pulls: usize,
}

impl EventSource for Sim {
    type Error = String;

    fn subscribe(&mut self, _filter: &str, _qos: u32, _depth: u32) -> Result<u64, String> {
        self.subscribed.push(1);
        Ok(1)
    }

    fn next_event(&mut self, _sub: u64) -> Result<Option<RawEvent>, String> {
        self.pulls += 1;
        if self.fail_after.is_some_and(|n| self.pulls > n) {
            return Err("the broker went away".to_string());
        }
        Ok(self.queue.pop_front())
    }

    fn stats(&mut self, _sub: u64) -> Result<BrokerStats, String> {
        Ok(BrokerStats::default())
    }

    fn ack(&mut self, _sub: u64, sequence: u64) -> Result<(), String> {
        self.acks.push(sequence);
        Ok(())
    }

    fn unsubscribe(&mut self, sub: u64) -> Result<(), String> {
        self.unsubscribed.push(sub);
        Ok(())
    }
}

#[test]
fn a_drain_pulls_a_batch_and_says_when_there_is_more() {
    let mut sim = Sim::default();
    for i in 0..10 {
        sim.queue
            .push_back(raw("time/tick", 5, i, tick_payload(i as i64)));
    }
    let mut config = Config::new("time/tick");
    config.drain_batch = 4;
    let mut l = Listener::start(&mut sim, config).unwrap();
    assert_eq!(l.subscription(), Some(1));
    let reg = Registry::default();
    let d = l.drain(&mut sim, 1000, &reg).unwrap();
    assert_eq!((d.pulled, d.more), (4, true));
    let d = l.drain(&mut sim, 1000, &reg).unwrap();
    assert_eq!((d.pulled, d.more), (4, true));
    let d = l.drain(&mut sim, 1000, &reg).unwrap();
    assert_eq!((d.pulled, d.more), (2, false));
    assert_eq!(l.received(), 10);
    assert!(sim.acks.is_empty(), "buffered is not acked");
    l.stop(&mut sim).unwrap();
    assert_eq!(sim.unsubscribed, [1]);
    assert_eq!(l.subscription(), None);
    l.stop(&mut sim).unwrap();
    assert_eq!(sim.unsubscribed, [1], "stop is idempotent");
    assert_eq!(l.drain(&mut sim, 1000, &reg).unwrap().pulled, 0);
}

#[test]
fn a_reliable_subscription_acks_what_it_pulled_even_when_capped() {
    let mut sim = Sim::default();
    for i in 1..=6 {
        sim.queue
            .push_back(raw("time/tick", 5, i, tick_payload(i as i64)));
    }
    let mut config = Config::new("time/tick");
    config.qos = 3;
    config.max_per_sec = 2;
    let mut l = Listener::start(&mut sim, config).unwrap();
    l.drain(&mut sim, 1000, &Registry::default()).unwrap();
    assert_eq!(
        sim.acks,
        [6],
        "acked up to the last pulled, though 4 were capped"
    );
    assert_eq!(l.strip(None).dropped_by_viewer, 4);
    assert!(l.accounted());
}

#[test]
fn a_failing_source_ends_the_drain_with_the_error_after_acking() {
    let mut sim = Sim::default();
    for i in 1..=5 {
        sim.queue
            .push_back(raw("time/tick", 5, i, tick_payload(i as i64)));
    }
    sim.fail_after = Some(3);
    let mut config = Config::new("time/tick");
    config.qos = 3;
    let mut l = Listener::start(&mut sim, config).unwrap();
    let err = l.drain(&mut sim, 1000, &Registry::default()).unwrap_err();
    assert_eq!(err, "the broker went away");
    assert_eq!(l.received(), 3);
    assert_eq!(sim.acks, [3]);
}

#[test]
fn peek_reads_what_is_ready_and_unsubscribes() {
    let mut sim = Sim::default();
    sim.queue.push_back(raw(
        "system/health/summary",
        5,
        1,
        topics::wrap(b"x").unwrap(),
    ));
    let events = peek(
        &mut sim,
        "system/health/summary",
        4,
        1000,
        &Registry::default(),
    )
    .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(sim.unsubscribed, [1]);
    // An error while reading still closes the subscription.
    let mut sim = Sim {
        fail_after: Some(0),
        ..Sim::default()
    };
    assert!(peek(&mut sim, "x", 4, 1000, &Registry::default()).is_err());
    assert_eq!(sim.unsubscribed, [1]);
}

#[test]
fn the_strip_carries_the_brokers_numbers_beside_the_viewers() {
    let l = Listener::offline(Config::new("#"));
    let strip = l.strip(Some(BrokerStats {
        qos: 3,
        depth: 8,
        queued: 2,
        delivered: 40,
        matched: 45,
        drops: 3,
    }));
    assert_eq!(strip.qos, "reliable");
    assert_eq!(
        (
            strip.depth,
            strip.queued,
            strip.delivered,
            strip.matched,
            strip.drops
        ),
        (8, 2, 40, 45, 3)
    );
    let blank = l.strip(None);
    assert_eq!(blank.qos, "buffered");
    assert_eq!(blank.depth, 16);
}

/// A producer that never runs out: every pull makes a fresh event.
struct Firehose {
    sequence: u64,
    limit: u64,
    payloads: Vec<Vec<u8>>,
}

impl EventSource for Firehose {
    type Error = String;

    fn subscribe(&mut self, _: &str, _: u32, _: u32) -> Result<u64, String> {
        Ok(1)
    }

    fn next_event(&mut self, _: u64) -> Result<Option<RawEvent>, String> {
        if self.sequence >= self.limit {
            return Ok(None);
        }
        self.sequence += 1;
        let payload = self.payloads[(self.sequence % self.payloads.len() as u64) as usize].clone();
        Ok(Some(raw(
            "time/tick",
            self.sequence % 40,
            self.sequence,
            payload,
        )))
    }

    fn stats(&mut self, _: u64) -> Result<BrokerStats, String> {
        Ok(BrokerStats::default())
    }

    fn ack(&mut self, _: u64, _: u64) -> Result<(), String> {
        Ok(())
    }

    fn unsubscribe(&mut self, _: u64) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn soak_millions_of_events_through_a_slow_consumer_stay_bounded() {
    let total = cases(2_000_000);
    let mut source = Firehose {
        sequence: 0,
        limit: total,
        payloads: (0..8).map(tick_payload).collect(),
    };
    let mut config = Config::new("time/tick");
    config.ring = 500;
    config.max_per_sec = 100;
    config.drain_batch = 128;
    let mut l = Listener::start(&mut source, config).unwrap();
    let reg = registry(&[("os.lazy.timed", 5)], 0);
    let mut now = 0u64;
    let mut peak_ring = 0;
    let mut peak_bytes = 0usize;
    let mut drains = 0u64;
    loop {
        // The consumer wakes once a millisecond and takes one batch.
        now += 1;
        let d = l.drain(&mut source, now, &reg).unwrap();
        drains += 1;
        peak_ring = peak_ring.max(l.len());
        if drains.is_multiple_of(997) {
            let bytes: usize = l
                .visible()
                .map(|e| e.topic.len() + e.summary().len() + 128)
                .sum();
            peak_bytes = peak_bytes.max(bytes);
            l.pause();
            l.resume();
        }
        if d.pulled == 0 {
            break;
        }
    }
    assert_eq!(l.received(), total);
    assert!(peak_ring <= 500, "ring grew to {peak_ring}");
    assert!(peak_bytes < 500 * 400, "the ring held {peak_bytes} bytes");
    assert!(l.accounted(), "every event is stored, capped or missed");
    let strip = l.strip(None);
    assert!(
        strip.dropped_by_viewer > total / 2,
        "the cap did its job: {strip:?}"
    );
    assert_eq!(l.pinned().len(), 0);
}
