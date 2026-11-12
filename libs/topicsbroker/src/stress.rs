//! Soak tests: many owners driving random publish / subscribe / unsubscribe /
//! pull / ack cycles, with the ledger audited against the tables after every
//! step and required to return exactly to zero when everything is released.

use alloc::vec;
use alloc::vec::Vec;

use crate::tests::{filter, B};
use crate::*;

/// A tiny deterministic generator (xorshift), so a failure replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const FILTERS: [&str; 6] = ["t/#", "t/+", "t/a", "t/b/#", "u/+/x", "#"];
const TOPICS: [&str; 6] = ["t/a", "t/b", "t/b/c", "u/q/x", "u/r/x", "v"];

fn qos(rng: &mut Rng) -> Qos {
    match rng.below(4) {
        0 => Qos::Latest,
        1 => Qos::Buffered(1 + rng.below(80) as u32),
        2 => Qos::Conflate,
        _ => Qos::Reliable,
    }
}

/// Owners `1..=OWNERS`; the first few are platform tasks.
const OWNERS: u64 = 40;

fn platform(owner: u64) -> bool {
    owner <= 4
}

/// What must hold at every step, whatever happened.
fn invariants(broker: &B, full: bool) {
    if full {
        broker.audit().expect("ledger matches the tables");
    }
    assert!(broker.bytes_used() <= MAX_BYTES);
    let apps: usize = (1..=OWNERS)
        .filter(|o| !platform(*o))
        .map(|o| broker.bytes_of(o))
        .sum();
    assert!(
        apps <= MAX_BYTES - SYSTEM_RESERVE,
        "apps passed the reserve"
    );
    for owner in 1..=OWNERS {
        let cap = if platform(owner) {
            SYSTEM_OWNER_BYTES
        } else {
            OWNER_BYTES
        };
        assert!(broker.bytes_of(owner) <= cap, "owner {owner} over its cap");
        assert!(broker.owned_by(owner) <= MAX_PER_OWNER);
    }
}

fn step(broker: &mut B, rng: &mut Rng, bells: &mut u32) {
    let owner = 1 + rng.below(OWNERS);
    match rng.below(10) {
        0 | 1 => {
            let f = filter(FILTERS[rng.below(FILTERS.len() as u64) as usize]);
            if let Ok(id) = broker.subscribe(owner, platform(owner), f, qos(rng)) {
                if rng.below(2) == 0 {
                    *bells += 1;
                    let _ = broker.set_bell(id, owner, *bells);
                }
                // A lost reply rolls the subscription back.
                let learned = rng.below(5) != 0;
                broker.settle_created(Some(id), owner, learned);
            }
        }
        2 => {
            let mine: Vec<u64> = broker
                .subscriptions
                .iter()
                .filter(|s| s.owner == owner)
                .map(|s| s.id)
                .collect();
            if let Some(id) = mine.get(rng.below(mine.len().max(1) as u64) as usize) {
                broker.remove(*id, owner).expect("own subscription");
            }
        }
        3..=6 => {
            let topic = TOPICS[rng.below(TOPICS.len() as u64) as usize];
            let len = match rng.below(4) {
                0 => 1,
                1 => 200,
                2 => 3000,
                _ => 8192,
            };
            let retained = rng.below(4) == 0;
            let topic = if retained {
                // Spread retained values over many topics so budgets bind.
                alloc::format!("{topic}/{}", rng.below(90))
            } else {
                alloc::string::String::from(topic)
            };
            if !valid_topic(&topic) {
                return;
            }
            let _ = broker.publish(&topic, owner, platform(owner), vec![3u8; len], retained);
            let _ = broker.satisfy(|event| Some(event.sequence));
        }
        7 => {
            let mine: Vec<(u64, bool)> = broker
                .subscriptions
                .iter()
                .filter(|s| s.owner == owner)
                .map(|s| (s.id, s.qos == Qos::Reliable))
                .collect();
            if let Some((id, reliable)) = mine.get(rng.below(mine.len().max(1) as u64) as usize) {
                let head = broker
                    .subscriptions
                    .iter()
                    .find(|s| s.id == *id)
                    .and_then(|s| s.peek().map(|e| e.sequence));
                if let Some(sequence) = head {
                    broker.commit(*id, sequence);
                    if *reliable {
                        broker.ack(*id, owner, sequence).expect("own subscription");
                    }
                }
            }
        }
        8 => {
            let mine: Vec<u64> = broker
                .subscriptions
                .iter()
                .filter(|s| s.owner == owner)
                .map(|s| s.id)
                .collect();
            if let Some(id) = mine.get(rng.below(mine.len().max(1) as u64) as usize) {
                let poll = rng.below(2) == 0;
                let _ = broker.pull(*id, owner, poll, Some(rng.next()));
            }
        }
        _ => {
            let _ = broker.bells_due();
        }
    }
}

#[test]
fn random_traffic_keeps_the_ledger_exact_and_within_budget() {
    for seed in 1..=4u64 {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ seed);
        let mut broker = B::new();
        let mut bells = 0;
        for n in 0..30_000 {
            step(&mut broker, &mut rng, &mut bells);
            // The audit walks every queue, so it runs every few steps; the
            // budget checks run on every one.
            invariants(&broker, n % 16 == 0);
        }
        // Release every subscription: only the retained values remain.
        let all: Vec<(u64, u64)> = broker
            .subscriptions
            .iter()
            .map(|s| (s.id, s.owner))
            .collect();
        for (id, owner) in all {
            broker.remove(id, owner).expect("remove");
        }
        invariants(&broker, true);
        assert!(broker.subscriptions.is_empty() && broker.pending.is_empty());
        let retained: usize = (1..=OWNERS).map(|o| broker.bytes_of(o)).sum();
        assert_eq!(retained, broker.bytes_used(), "only retained bytes remain");
    }
}

#[test]
fn subscribe_publish_unsubscribe_cycles_return_to_exactly_zero() {
    let mut broker = B::new();
    let mut rng = Rng(42);
    for round in 0..20_000u64 {
        let owner = 1 + round % OWNERS;
        let id = broker
            .subscribe(owner, platform(owner), filter("t/#"), qos(&mut rng))
            .expect("subscribe");
        for topic in TOPICS.iter().take(3) {
            let len = 1 + rng.below(8192) as usize;
            broker
                .publish(topic, owner + 100, false, vec![1u8; len], false)
                .expect("non-retained publish");
        }
        assert!(broker.bytes_of(owner) > 0 || broker.subscriptions[0].queued() == 0);
        broker.remove(id, owner).expect("remove");
        assert_eq!(broker.bytes_used(), 0, "round {round}");
        assert_eq!(broker.bytes_of(owner), 0);
    }
    broker.audit().unwrap();
}

#[test]
fn many_slow_subscribers_cannot_push_the_broker_past_its_budget() {
    let mut broker = B::new();
    // 256 subscriptions across the table, nobody ever polls.
    for slot in 0..MAX_SUBSCRIPTIONS as u64 {
        let owner = 10 + slot / 8;
        broker
            .subscribe(owner, false, filter("#"), Qos::Buffered(64))
            .expect("subscribe");
    }
    for round in 0..300 {
        broker
            .publish("t/a", 1, true, vec![9u8; 8192], round % 7 == 0)
            .expect("a platform publisher is never refused by slow readers");
        assert!(broker.bytes_used() <= MAX_BYTES - SYSTEM_RESERVE + SYSTEM_OWNER_BYTES);
    }
    for owner in 10..10 + (MAX_SUBSCRIPTIONS as u64 / 8) {
        assert!(broker.bytes_of(owner) <= OWNER_BYTES);
    }
    broker.audit().unwrap();
}
