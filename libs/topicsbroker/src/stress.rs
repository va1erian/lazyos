//! Soak tests: many task slots, some of them changing tenant under the
//! broker's feet (an exited task's slot is reused by another app), driving
//! random publish / subscribe / unsubscribe / pull / ack / reap cycles, with
//! the ledger audited against the tables after every step and required to
//! return exactly to zero when everything is released.

use alloc::vec;
use alloc::vec::Vec;

use crate::tenant::Tenant;
use crate::tests::{filter, sys, B};
use crate::*;

/// A tiny deterministic generator (xorshift), so a failure replays.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub(crate) fn below(&mut self, n: u64) -> u64 {
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

/// Task slots `1..=SLOTS`.
const SLOTS: u64 = 40;
/// Apps a slot may belong to: fewer than the slots, so tenants have many
/// threads (the case the budgets are keyed for).
const APPS: u64 = 8;

/// An app tenant of the random world.
fn app_tenant(app: u64) -> Tenant {
    Tenant::App {
        label: 1 + app as u32,
        uid: 1000,
        session: 1,
    }
}

/// Who each slot currently is. Slots `1..=4` start as platform tasks, the
/// rest as threads of a few apps.
struct World {
    clients: Vec<Client>,
}

impl World {
    fn new() -> Self {
        let clients = (0..=SLOTS)
            .map(|slot| {
                if slot <= 4 {
                    sys(slot)
                } else {
                    Client::new(slot, app_tenant(slot % APPS))
                }
            })
            .collect();
        World { clients }
    }

    /// The task in `slot` exits and an unrelated one takes the slot: another
    /// app, or a platform task. Whatever the old one left stays behind.
    fn reuse(&mut self, slot: u64, rng: &mut Rng) {
        let tenant = match rng.below(6) {
            0 => Tenant::Platform(slot),
            n => app_tenant(n + rng.below(APPS)),
        };
        self.clients[slot as usize] = Client::new(slot, tenant);
    }
}

/// What must hold at every step, whatever happened.
fn invariants(broker: &B) {
    broker.audit().expect("ledger matches the tables");
    assert!(broker.bytes_used() <= MAX_BYTES);
    let mut apps = 0;
    for (tenant, account) in &broker.accounts {
        if tenant.is_platform() {
            assert!(account.bytes <= SYSTEM_TENANT_BYTES, "{tenant:?} over cap");
            assert!(broker.owned_by(tenant) <= SYSTEM_TENANT_SUBSCRIPTIONS);
        } else {
            apps += account.bytes;
            assert!(account.bytes <= TENANT_BYTES, "{tenant:?} over its cap");
            assert!(account.retained <= TENANT_RETAINED, "{tenant:?} retained");
        }
    }
    assert!(
        apps <= MAX_BYTES - SYSTEM_RESERVE,
        "apps passed the reserve"
    );
    let mut held = alloc::collections::BTreeMap::new();
    for sub in &broker.subscriptions {
        *held.entry(sub.tenant).or_insert(0usize) += 1;
    }
    for (tenant, count) in held {
        if !tenant.is_platform() {
            assert!(count <= TENANT_SUBSCRIPTIONS, "{tenant:?} subscriptions");
        }
    }
    let app_subs = broker
        .subscriptions
        .iter()
        .filter(|s| !s.tenant.is_platform())
        .count();
    assert!(app_subs <= MAX_SUBSCRIPTIONS - SYSTEM_SUBSCRIPTION_RESERVE);
    let app_rows = broker
        .retained
        .iter()
        .filter(|r| !r.tenant.is_platform())
        .count();
    assert!(app_rows <= MAX_TOPICS - SYSTEM_TOPIC_RESERVE);
}

/// Subscription ids the client in `slot` may touch.
fn mine(broker: &B, client: Client) -> Vec<u64> {
    broker
        .subscriptions
        .iter()
        .filter(|s| s.client() == client)
        .map(|s| s.id)
        .collect()
}

/// Encode like `messengerd` does, except that a full-size event "does not
/// fit a reply" (the case that drops an undeliverable head).
fn encode(event: &Event) -> Option<u64> {
    (event.payload.len() < 8192).then_some(event.sequence)
}

fn step(broker: &mut B, world: &mut World, rng: &mut Rng, bells: &mut u32) {
    let slot = 1 + rng.below(SLOTS);
    let client = world.clients[slot as usize];
    match rng.below(12) {
        0 | 1 => {
            let f = filter(FILTERS[rng.below(FILTERS.len() as u64) as usize]);
            if let Ok(id) = broker.subscribe(client, f, qos(rng)) {
                if rng.below(2) == 0 {
                    *bells += 1;
                    let _ = broker.set_bell(id, client, *bells);
                }
                // A lost reply rolls the subscription back.
                let learned = rng.below(5) != 0;
                broker.settle_created(Some(id), client, learned);
            }
        }
        2 => {
            let ids = mine(broker, client);
            if let Some(id) = ids.get(rng.below(ids.len().max(1) as u64) as usize) {
                broker.remove(*id, client).expect("own subscription");
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
            let _ = broker.publish(&topic, client, vec![3u8; len], retained);
            let _ = broker.satisfy(encode);
        }
        7 => {
            let ids = mine(broker, client);
            if let Some(id) = ids.get(rng.below(ids.len().max(1) as u64) as usize) {
                let found = broker.subscriptions.iter().find(|s| s.id == *id);
                let reliable = found.is_some_and(|s| s.qos == Qos::Reliable);
                if let Some(sequence) = found.and_then(|s| s.peek().map(|e| e.sequence)) {
                    broker.commit(*id, sequence);
                    if reliable {
                        broker.ack(*id, client, sequence).expect("own subscription");
                    }
                }
            }
        }
        8 => {
            let ids = mine(broker, client);
            if let Some(id) = ids.get(rng.below(ids.len().max(1) as u64) as usize) {
                let poll = rng.below(2) == 0;
                if let Ok(Pulled::Head(index)) = broker.pull(*id, client, poll, Some(rng.next())) {
                    // The reply could not be encoded: the head is dropped.
                    let head = broker.subscriptions[index].peek().map(|e| e.sequence);
                    if let Some(sequence) = head.filter(|_| rng.below(3) == 0) {
                        broker.drop_undeliverable(*id, sequence);
                    }
                }
            }
        }
        9 => {
            // The doorbell's far end closed: `ring_bells` reaps it.
            let ids: Vec<u64> = broker.subscriptions.iter().map(|s| s.id).collect();
            if let Some(id) = ids.get(rng.below(ids.len().max(1) as u64) as usize) {
                broker.reap(*id);
            }
        }
        10 => world.reuse(slot, rng),
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
        let mut world = World::new();
        let mut bells = 0;
        for _ in 0..30_000 {
            step(&mut broker, &mut world, &mut rng, &mut bells);
            // The audit walks every queue, and it runs after every step.
            invariants(&broker);
        }
        // Release every subscription: only the retained values remain.
        let all: Vec<u64> = broker.subscriptions.iter().map(|s| s.id).collect();
        for id in all {
            broker.reap(id);
        }
        invariants(&broker);
        assert!(broker.subscriptions.is_empty());
        let retained: usize = broker.accounts.values().map(|a| a.bytes).sum();
        assert_eq!(retained, broker.bytes_used(), "only retained bytes remain");
    }
}

#[test]
fn subscribe_publish_unsubscribe_cycles_return_to_exactly_zero() {
    let mut broker = B::new();
    let mut rng = Rng(42);
    for round in 0..20_000u64 {
        let owner = 1 + round % SLOTS;
        let client = World::new().clients[owner as usize];
        let id = broker
            .subscribe(client, filter("t/#"), qos(&mut rng))
            .expect("subscribe");
        for topic in TOPICS.iter().take(3) {
            let len = 1 + rng.below(8192) as usize;
            broker
                .publish(topic, crate::tests::app(owner + 100), vec![1u8; len], false)
                .expect("non-retained publish");
        }
        assert!(broker.bytes_of(&client.tenant) > 0 || broker.subscriptions[0].queued() == 0);
        broker.remove(id, client).expect("remove");
        assert_eq!(broker.bytes_used(), 0, "round {round}");
        assert_eq!(broker.bytes_of(&client.tenant), 0);
    }
    broker.audit().unwrap();
}

#[test]
fn many_slow_subscribers_cannot_push_the_broker_past_its_budget() {
    let mut broker = B::new();
    // The apps' part of the table, nobody ever polls.
    let mut tenants = 0;
    for slot in 0..(MAX_SUBSCRIPTIONS - SYSTEM_SUBSCRIPTION_RESERVE) as u64 {
        let client = crate::tests::app(10 + slot / TENANT_SUBSCRIPTIONS as u64);
        tenants = tenants.max(slot / TENANT_SUBSCRIPTIONS as u64 + 1);
        broker
            .subscribe(client, filter("#"), Qos::Buffered(64))
            .expect("subscribe");
    }
    for round in 0..300 {
        broker
            .publish("t/a", sys(1), vec![9u8; 8192], round % 7 == 0)
            .expect("a platform publisher is never refused by slow readers");
        assert!(broker.bytes_used() <= MAX_BYTES - SYSTEM_RESERVE + SYSTEM_TENANT_BYTES);
    }
    for tenant in 0..tenants {
        assert!(broker.bytes_of(&crate::tests::t(10 + tenant)) <= TENANT_BYTES);
    }
    broker.audit().unwrap();
}
