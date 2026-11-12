//! Unit tests: queue byte accounting, per-tenant limits, the budget and its
//! system reserve, rollback and the poll early return.

use alloc::vec;
use alloc::vec::Vec;

use crate::*;

pub(crate) type B = Broker<u32>;

pub(crate) fn filter(text: &str) -> Filter {
    Filter::parse(text).expect("valid filter")
}

/// A client of app `n`: slot `n`, and a tenant of its own.
pub(crate) fn app(n: u64) -> Client {
    Client::new(
        n,
        Tenant::App {
            label: n as u32,
            uid: 1000,
            session: 1,
        },
    )
}

/// A platform task in slot `n`.
pub(crate) fn sys(n: u64) -> Client {
    Client::new(n, Tenant::Platform(n))
}

/// The tenant of [`app`]`(n)`.
pub(crate) fn t(n: u64) -> Tenant {
    app(n).tenant
}

pub(crate) fn sub(broker: &mut B, owner: u64, text: &str, qos: Qos) -> u64 {
    broker
        .subscribe(app(owner), filter(text), qos)
        .expect("subscribe")
}

pub(crate) fn publish(broker: &mut B, topic: &str, who: u64, len: usize, retained: bool) {
    broker
        .publish(topic, app(who), vec![7u8; len], retained)
        .expect("publish");
}

/// One event's cost for a `len`-byte payload on `topic`.
fn cost(topic: &str, len: usize) -> usize {
    len + topic.len() + 64
}

fn queued(broker: &B, id: u64) -> u64 {
    broker
        .subscriptions
        .iter()
        .find(|s| s.id == id)
        .expect("subscription")
        .queued()
}

fn drops(broker: &B, id: u64) -> u64 {
    broker
        .subscriptions
        .iter()
        .find(|s| s.id == id)
        .expect("subscription")
        .drops
}

/// Pop everything a subscription holds the way a poll does.
fn drain(broker: &mut B, id: u64, owner: u64) {
    while let Some(sequence) = broker
        .subscriptions
        .iter()
        .find(|s| s.id == id)
        .and_then(|s| s.peek().map(|e| e.sequence))
    {
        broker.commit(id, sequence);
        if broker.ack(id, app(owner), sequence).is_err() {
            break;
        }
        // A reliable queue pops on ack, the rest on commit: either way the
        // head must have moved.
        assert!(
            broker
                .subscriptions
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.peek().map(|e| e.sequence))
                != Some(sequence)
        );
    }
}

#[test]
fn push_commit_retires_exactly_what_was_charged() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "a/b", Qos::Buffered(4));
    publish(&mut broker, "a/b", 9, 100, false);
    publish(&mut broker, "a/b", 9, 200, false);
    assert_eq!(broker.bytes_of(&t(1)), cost("a/b", 100) + cost("a/b", 200));
    assert_eq!(broker.bytes_used(), broker.bytes_of(&t(1)));
    let head = broker.subscriptions[0].peek().unwrap().sequence;
    broker.commit(id, head);
    assert_eq!(broker.bytes_of(&t(1)), cost("a/b", 200));
    drain(&mut broker, id, 1);
    assert_eq!(broker.bytes_used(), 0);
    broker.audit().unwrap();
}

#[test]
fn a_late_commit_does_not_pop_a_newer_event() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "a", Qos::Buffered(4));
    publish(&mut broker, "a", 9, 10, false);
    publish(&mut broker, "a", 9, 10, false);
    broker.commit(id, 9999);
    assert_eq!(queued(&broker, id), 2);
    broker.audit().unwrap();
}

#[test]
fn overflow_and_latest_replace_by_net_change() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "a", Qos::Buffered(2));
    for _ in 0..5 {
        publish(&mut broker, "a", 9, 50, false);
    }
    assert_eq!(queued(&broker, id), 2);
    assert_eq!(drops(&broker, id), 3);
    assert_eq!(broker.bytes_of(&t(1)), 2 * cost("a", 50));
    let latest = sub(&mut broker, 2, "a", Qos::Latest);
    publish(&mut broker, "a", 9, 80, false);
    publish(&mut broker, "a", 9, 90, false);
    assert_eq!(queued(&broker, latest), 1);
    assert_eq!(broker.bytes_of(&t(2)), cost("a", 90));
    broker.audit().unwrap();
}

#[test]
fn conflate_replaces_per_publisher_and_charges_the_difference() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "a", Qos::Conflate);
    publish(&mut broker, "a", 5, 100, false);
    publish(&mut broker, "a", 6, 10, false);
    publish(&mut broker, "a", 5, 300, false);
    assert_eq!(queued(&broker, id), 2);
    assert_eq!(drops(&broker, id), 1);
    assert_eq!(broker.bytes_of(&t(1)), cost("a", 300) + cost("a", 10));
    broker.audit().unwrap();
}

#[test]
fn reliable_pops_only_on_ack() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "a", Qos::Reliable);
    publish(&mut broker, "a", 9, 10, false);
    publish(&mut broker, "a", 9, 10, false);
    let first = broker.subscriptions[0].peek().unwrap().sequence;
    broker.commit(id, first);
    assert_eq!(queued(&broker, id), 2, "delivery does not pop");
    assert_eq!(broker.ack(id, app(2), first), Err(Refusal::NotOwner));
    broker.ack(id, app(1), first).unwrap();
    assert_eq!(queued(&broker, id), 1);
    assert_eq!(broker.bytes_of(&t(1)), cost("a", 10));
    broker.audit().unwrap();
}

#[test]
fn an_undeliverable_head_is_dropped_with_its_bytes() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "a", Qos::Reliable);
    broker.pull(id, app(1), false, Some(5)).unwrap();
    publish(&mut broker, "a", 9, 10, false);
    let wakes = broker.satisfy(|_| None::<u8>);
    assert!(wakes.is_empty());
    assert_eq!(queued(&broker, id), 0);
    assert_eq!(drops(&broker, id), 1);
    assert_eq!(broker.bytes_used(), 0);
    broker.audit().unwrap();
}

#[test]
fn unsubscribe_returns_every_queued_byte_and_the_bell() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "a/#", Qos::Buffered(8));
    broker.set_bell(id, app(1), 77).unwrap();
    publish(&mut broker, "a/x", 9, 500, false);
    publish(&mut broker, "a/y", 9, 500, false);
    assert!(broker.bytes_used() > 1000);
    assert_eq!(broker.remove(id, app(2)), Err(Refusal::NotOwner));
    assert_eq!(broker.remove(id, app(1)), Ok(Some(77)));
    assert_eq!(broker.bytes_used(), 0);
    assert_eq!(broker.bytes_of(&t(1)), 0);
    assert_eq!(broker.remove(id, app(1)), Err(Refusal::NotFound));
    broker.audit().unwrap();
}

#[test]
fn subscription_count_is_limited_per_owner_and_in_total() {
    let mut broker = B::new();
    for _ in 0..TENANT_SUBSCRIPTIONS {
        sub(&mut broker, 1, "a", Qos::Latest);
    }
    let over = broker.subscribe(app(1), filter("a"), Qos::Latest);
    assert_eq!(over, Err(Refusal::Busy));
    // Other tenants fill the apps' part of the table; the next app is refused
    // for room, but a platform task still has the reserve.
    let mut owner = 2;
    while broker.subscriptions.len() < MAX_SUBSCRIPTIONS - SYSTEM_SUBSCRIPTION_RESERVE {
        sub(&mut broker, owner, "a", Qos::Latest);
        if broker.owned_by(&t(owner)) == TENANT_SUBSCRIPTIONS {
            owner += 1;
        }
    }
    let full = broker.subscribe(app(1000), filter("a"), Qos::Latest);
    assert_eq!(full, Err(Refusal::NoRoom));
    for slot in 0..SYSTEM_SUBSCRIPTION_RESERVE as u64 {
        broker
            .subscribe(sys(slot), filter("a"), Qos::Latest)
            .expect("the reserve is for platform tasks");
    }
    let table = broker.subscribe(sys(9999), filter("a"), Qos::Latest);
    assert_eq!(table, Err(Refusal::NoRoom));
}

#[test]
fn retained_values_are_charged_to_their_publisher() {
    let mut broker = B::new();
    publish(&mut broker, "s/a", 3, 1000, true);
    assert_eq!(broker.bytes_of(&t(3)), cost("s/a", 1000));
    // Same publisher replaces: the net change, not a second copy.
    publish(&mut broker, "s/a", 3, 400, true);
    assert_eq!(broker.bytes_of(&t(3)), cost("s/a", 400));
    // Another publisher takes the topic over: the charge moves.
    publish(&mut broker, "s/a", 4, 100, true);
    assert_eq!(broker.bytes_of(&t(3)), 0);
    assert_eq!(broker.bytes_of(&t(4)), cost("s/a", 100));
    broker.audit().unwrap();
}

#[test]
fn a_new_subscriber_is_charged_for_the_retained_replay() {
    let mut broker = B::new();
    publish(&mut broker, "s/a", 3, 1000, true);
    let id = sub(&mut broker, 8, "s/#", Qos::Latest);
    assert_eq!(queued(&broker, id), 1);
    assert_eq!(broker.bytes_of(&t(8)), cost("s/a", 1000));
    assert!(broker.subscriptions[0].peek().unwrap().retained);
    broker.audit().unwrap();
}

#[test]
fn an_app_cannot_hold_more_than_its_allowance_in_retained_values() {
    let mut broker = B::new();
    let mut accepted = 0;
    for n in 0..64 {
        let topic = alloc::format!("r/{n}");
        match broker.publish(&topic, app(5), vec![1u8; 8192], true) {
            Ok(_) => accepted += 1,
            Err(refusal) => {
                assert_eq!(refusal, Refusal::NoRoom);
                break;
            }
        }
    }
    assert_eq!(accepted, TENANT_RETAINED);
    assert!(broker.bytes_of(&t(5)) <= TENANT_BYTES);
    // Others are unaffected, and so is a non-retained publish of the same app.
    publish(&mut broker, "other", 6, 8192, true);
    assert!(broker
        .publish("r/plain", app(5), vec![1u8; 8192], false)
        .is_ok());
    broker.audit().unwrap();
}

#[test]
fn an_app_is_limited_in_retained_count() {
    let mut broker = B::new();
    for n in 0..TENANT_RETAINED {
        publish(&mut broker, &alloc::format!("c/{n}"), 5, 1, true);
    }
    let over = broker.publish("c/over", app(5), vec![1], true);
    assert_eq!(over, Err(Refusal::NoRoom));
    // Replacing one of its own values is not a new one.
    publish(&mut broker, "c/0", 5, 2, true);
    // A platform task has no count limit.
    assert!(broker.publish("c/sys", sys(5), vec![1], true).is_ok());
}

#[test]
fn queue_depth_overflow_never_refuses_the_publisher() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "big", Qos::Buffered(64));
    for _ in 0..200 {
        publish(&mut broker, "big", 9, 8192, false);
    }
    assert_eq!(drops(&broker, id), 136);
    assert_eq!(queued(&broker, id), 64);
    broker.audit().unwrap();
}

#[test]
fn a_queue_over_its_owners_allowance_sheds_its_oldest() {
    let mut broker = B::new();
    for n in 0..16 {
        sub(&mut broker, 1, &alloc::format!("t/{n}"), Qos::Buffered(64));
    }
    for round in 0..300 {
        for n in 0..16 {
            publish(&mut broker, &alloc::format!("t/{n}"), 9, 8192, false);
        }
        assert!(broker.bytes_of(&t(1)) <= TENANT_BYTES, "round {round}");
    }
    broker.audit().unwrap();
}

#[test]
fn apps_together_cannot_take_the_system_reserve() {
    let mut broker = B::new();
    // Many apps each fill their own allowance with retained values.
    let apps = MAX_BYTES / TENANT_BYTES + 4;
    for tenant in 0..apps as u64 {
        for n in 0..64 {
            let topic = alloc::format!("app{tenant}/{n}");
            let _ = broker.publish(&topic, app(100 + tenant), vec![1u8; 8192], true);
        }
    }
    assert!(broker.bytes_used() <= MAX_BYTES - SYSTEM_RESERVE);
    // A platform task still has its reserve for a retained value...
    assert!(broker
        .publish("system/health/x", sys(1), vec![1u8; 8192], true)
        .is_ok());
    // ...and an app is told no, while its non-retained publish still works.
    assert_eq!(
        broker.publish("app/late", app(999), vec![1u8; 8192], true),
        Err(Refusal::NoRoom)
    );
    assert!(broker
        .publish("app/late", app(999), vec![1u8; 8192], false)
        .is_ok());
    // Subscribing is never refused for bytes.
    assert!(broker
        .subscribe(app(999), filter("app0/#"), Qos::Latest)
        .is_ok());
    broker.audit().unwrap();
}

#[test]
fn platform_queues_use_the_larger_allowance() {
    let mut broker = B::new();
    // Four deep queues each: more than an app's allowance, less than a
    // platform task's.
    for _ in 0..4 {
        sub(&mut broker, 1, "q", Qos::Buffered(64));
        broker
            .subscribe(sys(2), filter("q"), Qos::Buffered(64))
            .unwrap();
    }
    for _ in 0..100 {
        publish(&mut broker, "q", 9, 8192, false);
    }
    assert!(broker.bytes_of(&t(1)) <= TENANT_BYTES);
    assert!(broker.bytes_of(&t(1)) > TENANT_BYTES - cost("q", 8192));
    assert!(broker.bytes_of(&sys(2).tenant) > TENANT_BYTES);
    assert!(broker.bytes_of(&sys(2).tenant) <= SYSTEM_TENANT_BYTES);
    broker.audit().unwrap();
}

#[test]
fn subscribe_rolls_back_when_its_reply_is_lost() {
    let mut broker = B::new();
    publish(&mut broker, "s/a", 3, 600, true);
    let id = sub(&mut broker, 8, "s/#", Qos::Latest);
    broker.set_bell(id, app(8), 5).unwrap();
    assert!(broker.bytes_of(&t(8)) > 0);
    // The reply reached the caller: kept.
    assert_eq!(broker.settle_created(Some(id), app(8), true), None);
    assert_eq!(broker.subscriptions.len(), 1);
    // It did not (timeout, or a one-way subscribe): rolled back, bell handed
    // back to close, replay bytes returned.
    assert_eq!(broker.settle_created(Some(id), app(8), false), Some(5));
    assert!(broker.subscriptions.is_empty());
    assert_eq!(broker.bytes_of(&t(8)), 0);
    // Nothing was created: nothing to undo.
    assert_eq!(broker.settle_created(None, app(8), false), None);
    broker.audit().unwrap();
}

#[test]
fn rollback_cannot_remove_someone_elses_subscription() {
    let mut broker = B::new();
    let id = sub(&mut broker, 8, "a", Qos::Latest);
    assert_eq!(broker.settle_created(Some(id), app(9), false), None);
    assert_eq!(broker.subscriptions.len(), 1);
}

#[test]
fn an_empty_poll_returns_without_parking() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "a", Qos::Latest);
    broker.subscriptions[0].rung = true;
    assert_eq!(broker.pull(id, app(1), true, Some(4)), Ok(Pulled::Empty));
    assert!(broker.pending.is_empty(), "a poll never parks");
    assert!(!broker.subscriptions[0].rung, "the owner drained the queue");
    // A real wait parks, once per subscription.
    assert_eq!(broker.pull(id, app(1), false, Some(5)), Ok(Pulled::Parked));
    assert_eq!(broker.pull(id, app(1), false, Some(6)), Ok(Pulled::Parked));
    assert_eq!(broker.pending.len(), 1);
    assert_eq!(broker.pending[0].txn, 6);
    assert_eq!(broker.pull(id, app(1), false, None), Err(Refusal::Invalid));
    assert_eq!(broker.pull(id, app(2), true, None), Err(Refusal::NotOwner));
    assert_eq!(broker.pull(99, app(1), true, None), Err(Refusal::NotFound));
}

#[test]
fn a_parked_pull_is_woken_by_a_publish_and_committed_once() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "a", Qos::Latest);
    assert_eq!(broker.pull(id, app(1), false, Some(7)), Ok(Pulled::Parked));
    publish(&mut broker, "a", 9, 10, false);
    let wakes = broker.satisfy(|event| Some(event.sequence));
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].txn, 7);
    assert!(broker.pending.is_empty());
    // Not popped until the reply lands.
    assert_eq!(queued(&broker, id), 1);
    broker.commit(id, wakes[0].sequence);
    assert_eq!(broker.bytes_used(), 0);
    assert_eq!(broker.subscriptions[0].delivered, 1);
}

#[test]
fn bells_ring_once_until_drained() {
    let mut broker = B::new();
    let id = sub(&mut broker, 1, "a", Qos::Latest);
    broker.set_bell(id, app(1), 3).unwrap();
    assert!(broker.bells_due().is_empty());
    publish(&mut broker, "a", 9, 1, false);
    assert_eq!(broker.bells_due(), vec![(id, 3)]);
    assert!(broker.bells_due().is_empty());
    assert_eq!(broker.drop_bell(id), Some(3));
    assert_eq!(broker.drop_bell(id), None);
}

#[test]
fn list_counts_subscribers_and_marks_retained() {
    let mut broker = B::new();
    sub(&mut broker, 1, "a/+", Qos::Latest);
    publish(&mut broker, "a/b", 9, 1, true);
    publish(&mut broker, "c", 9, 1, false);
    let rows: Vec<_> = broker
        .list()
        .into_iter()
        .map(|r| (r.topic, r.subscribers, r.retained))
        .collect();
    assert_eq!(rows, vec![("a/b".into(), 1, true), ("c".into(), 0, false)]);
}

#[test]
fn filters_match_like_the_kernel_gate() {
    assert!(filter("a/+/c").matches("a/b/c"));
    assert!(!filter("a/+/c").matches("a/b/d"));
    assert!(filter("a/#").matches("a"));
    assert!(Filter::parse("a/#/b").is_none());
    assert!(valid_topic("a/b") && !valid_topic("a/+"));
}

#[test]
fn a_subscription_whose_bell_end_is_gone_is_reaped_with_its_bytes() {
    let mut broker = B::new();
    publish(&mut broker, "s/a", 3, 300, true);
    let id = sub(&mut broker, 8, "s/#", Qos::Latest);
    broker.set_bell(id, app(8), 6).unwrap();
    assert_eq!(broker.reap(id), Some(6));
    assert!(broker.subscriptions.is_empty());
    assert_eq!(broker.bytes_of(&t(8)), 0);
    assert_eq!(broker.reap(id), None);
    broker.audit().unwrap();
}
