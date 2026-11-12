//! Tenant tests: the budgets belong to an app as a whole (every thread and
//! process of it), a reused task slot starts clean, and platform tasks keep
//! their reserve whatever the apps do.

use alloc::format;
use alloc::vec;

use crate::tenant::Tenant;
use crate::tests::{app, filter, sys, B};
use crate::*;

/// The thread `thread` of one app (label 7): many slots, one tenant.
fn thread(thread: u64) -> Client {
    Client::new(
        1000 + thread,
        Tenant::App {
            label: 7,
            uid: 1000,
            session: 1,
        },
    )
}

#[test]
fn threads_of_one_app_share_one_subscription_budget() {
    let mut broker = B::new();
    let mut taken = 0;
    for slot in 0..4 * TENANT_SUBSCRIPTIONS as u64 {
        match broker.subscribe(thread(slot), filter("a"), Qos::Latest) {
            Ok(_) => taken += 1,
            Err(why) => assert_eq!(why, Refusal::Busy),
        }
    }
    assert_eq!(taken, TENANT_SUBSCRIPTIONS);
    assert_eq!(broker.owned_by(&thread(0).tenant), TENANT_SUBSCRIPTIONS);
    // Another app and a platform task are untouched.
    assert!(broker.subscribe(app(1), filter("a"), Qos::Latest).is_ok());
    assert!(broker.subscribe(sys(1), filter("a"), Qos::Latest).is_ok());
}

#[test]
fn threads_of_one_app_share_one_retained_budget() {
    let mut broker = B::new();
    for slot in 0..16u64 {
        for n in 0..TENANT_RETAINED {
            let topic = format!("app/seven/{slot}/{n}");
            let _ = broker.publish(&topic, thread(slot), vec![1], true);
        }
    }
    assert_eq!(broker.retained_by(&thread(0).tenant), TENANT_RETAINED);
    // Another app still publishes a retained value, so does a platform task.
    assert!(broker.publish("x/y", app(1), vec![1], true).is_ok());
    assert!(broker.publish("system/z", sys(1), vec![1], true).is_ok());
    broker.audit().unwrap();
}

#[test]
fn threads_of_one_app_share_one_byte_budget() {
    let mut broker = B::new();
    for slot in 0..16u64 {
        let id = broker
            .subscribe(thread(slot), filter("#"), Qos::Buffered(64))
            .expect("subscribe");
        assert!(id > 0);
    }
    for n in 0..200 {
        let _ = broker.publish(&format!("t/{n}"), thread(99), vec![1u8; 8192], n % 3 == 0);
    }
    assert!(broker.bytes_of(&thread(0).tenant) <= TENANT_BYTES);
    // Its neighbours' queues still have room: the app did not drain the pool.
    let neighbour = app(2);
    let id = broker
        .subscribe(neighbour, filter("t/#"), Qos::Latest)
        .unwrap();
    broker
        .publish("t/after", sys(1), vec![1u8; 8192], false)
        .unwrap();
    let head = broker
        .subscriptions
        .iter()
        .find(|s| s.id == id)
        .unwrap()
        .peek();
    assert_eq!(head.map(|e| e.topic.as_str()), Some("t/after"));
    broker.audit().unwrap();
}

#[test]
fn platform_keeps_its_rows_when_apps_have_filled_theirs() {
    let mut broker = B::new();
    // Many apps fill every subscription row and retained row they may take.
    for label in 0..64u64 {
        for n in 0..TENANT_SUBSCRIPTIONS.max(TENANT_RETAINED) {
            let client = app(label);
            let _ = broker.subscribe(client, filter("a"), Qos::Latest);
            let _ = broker.publish(&format!("a{label}/{n}"), client, vec![1], true);
        }
    }
    assert_eq!(
        broker.subscriptions.len(),
        MAX_SUBSCRIPTIONS - SYSTEM_SUBSCRIPTION_RESERVE
    );
    assert_eq!(broker.retained.len(), MAX_TOPICS - SYSTEM_TOPIC_RESERVE);
    assert_eq!(
        broker.subscribe(app(999), filter("a"), Qos::Latest),
        Err(Refusal::NoRoom)
    );
    assert_eq!(
        broker.publish("a999/x", app(999), vec![1], true),
        Err(Refusal::NoRoom)
    );
    // A platform task can still subscribe, and publish a new retained topic
    // that a late subscriber is replayed.
    broker
        .subscribe(sys(1), filter("a"), Qos::Latest)
        .expect("platform subscribe");
    broker
        .publish("system/new", sys(1), vec![1], true)
        .expect("platform retained");
    let late = broker
        .subscribe(sys(2), filter("system/new"), Qos::Latest)
        .unwrap();
    assert!(broker
        .subscriptions
        .iter()
        .any(|s| s.id == late && s.queued() == 1));
    broker.audit().unwrap();
}

#[test]
fn apps_cannot_fill_the_topic_list_and_hide_platform_topics() {
    let mut broker = B::new();
    for n in 0..MAX_TOPICS {
        let _ = broker.publish(&format!("junk/{n}"), app(n as u64 % 50), vec![1], false);
    }
    assert_eq!(broker.list().len(), MAX_TOPICS - SYSTEM_TOPIC_RESERVE);
    broker
        .publish("system/real", sys(1), vec![1], false)
        .unwrap();
    assert!(broker.list().iter().any(|row| row.topic == "system/real"));
}

#[test]
fn a_task_that_reuses_a_slot_inherits_no_charges_and_no_subscriptions() {
    let mut broker = B::new();
    let old = Client::new(5, app(1).tenant);
    let new = Client::new(5, app(2).tenant);
    // The old task fills its share and its retained rows, then exits.
    let mut ids = vec![];
    for n in 0..TENANT_SUBSCRIPTIONS {
        ids.push(
            broker
                .subscribe(old, filter("a/#"), Qos::Buffered(64))
                .unwrap(),
        );
        let _ = broker.publish(&format!("o/{n}"), old, vec![1u8; 8192], true);
    }
    assert!(broker.bytes_of(&old.tenant) > 0);
    // The new task, same slot and a different app, starts from nothing...
    assert_eq!(broker.bytes_of(&new.tenant), 0);
    assert_eq!(broker.owned_by(&new.tenant), 0);
    assert_eq!(broker.headroom(new.tenant), TENANT_BYTES);
    assert!(broker.subscribe(new, filter("n"), Qos::Latest).is_ok());
    assert!(broker.publish("n/x", new, vec![1], true).is_ok());
    // ...and cannot pull, ack, ring or remove what the old one left.
    assert_eq!(broker.pull(ids[0], new, true, None), Err(Refusal::NotOwner));
    assert_eq!(broker.ack(ids[0], new, 1), Err(Refusal::NotOwner));
    assert_eq!(broker.set_bell(ids[0], new, 1), Err(Refusal::NotOwner));
    assert_eq!(broker.remove(ids[0], new), Err(Refusal::NotOwner));
    // A platform task in that slot is a third identity as well.
    assert_eq!(broker.remove(ids[0], sys(5)), Err(Refusal::NotOwner));
    // The old tenant's own leftovers are cleaned the way messengerd does it
    // (the bell's EPIPE), and its budget comes back.
    for id in ids {
        broker.reap(id);
    }
    assert_eq!(broker.owned_by(&old.tenant), 0);
    broker.audit().unwrap();
}

#[test]
fn only_a_labelled_or_logged_in_task_is_an_app() {
    assert_eq!(Tenant::from_stamp(0, 0, 0, 9), Tenant::Platform(9));
    assert_eq!(Tenant::from_stamp(0, 1000, 0, 9), Tenant::Platform(9));
    for tenant in [
        Tenant::from_stamp(3, 0, 0, 9),
        Tenant::from_stamp(0, 0, 4, 9),
    ] {
        assert!(!tenant.is_platform());
    }
    // The slot is no part of an app's tenant.
    assert_eq!(
        Tenant::from_stamp(3, 1000, 4, 1),
        Tenant::from_stamp(3, 1000, 4, 2)
    );
}
