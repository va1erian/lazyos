//! The doorbell and the subscription lifecycle against the scripted broker.

use std::cell::RefCell;
use std::rc::Rc;

use libmessenger::Parcel;
use messenger_generated::os_lazy_messenger_topics_bell_v1 as bell;
use messenger_generated::os_lazy_messenger_topics_v1 as topics;

use super::fake::{ready, Fake, Shared};
use super::tests::{client, subscribed};
use super::{Qos, Subscriber, Wait, MAX_BELL_MESSAGES};

// -- The doorbell ------------------------------------------------------------

#[test]
fn a_bell_rings_once_per_batch_and_the_drain_rearms_it() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Buffered);
    let handle = c.attach_bell(id).unwrap();
    assert_eq!(c.doorbell_handles(), vec![handle]);
    assert_eq!(c.has_bell(id), Ok(true));
    assert!(c.consume_ready().ready.is_empty());

    c.transport.publish(id.0, "a/1", b"x");
    c.transport.publish(id.0, "a/2", b"y");
    let rung = c.consume_ready();
    assert_eq!(rung.ready, vec![id]);
    assert!(rung.lost.is_empty());
    assert!(c.consume_ready().ready.is_empty(), "the ring was consumed");

    // Not drained yet: the broker rings no more...
    c.transport.publish(id.0, "a/3", b"z");
    assert!(c.consume_ready().ready.is_empty());
    // ...until the drain finds the queue empty.
    assert_eq!(c.drain(id, 10).events.len(), 3);
    c.transport.publish(id.0, "a/4", b"w");
    assert_eq!(c.consume_ready().ready, vec![id]);
}

#[test]
fn attach_bell_is_asked_once() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    let first = c.attach_bell(id).unwrap();
    let again = c.attach_bell(id).unwrap();
    assert_eq!(first, again);
    let bells = c
        .transport
        .log
        .iter()
        .filter(|(m, _)| *m == topics::METHOD_BELL)
        .count();
    assert_eq!(bells, 1);
    assert!(c.transport.closed.is_empty());
}

#[test]
fn a_refused_bell_closes_both_ends() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    c.transport.fail.push_back((topics::METHOD_BELL, 1));
    assert_eq!(c.attach_bell(id), Err(-1));
    assert_eq!(c.has_bell(id), Ok(false));
    assert!(c.doorbell_handles().is_empty());
    assert_eq!(c.transport.closed.len(), 2);

    // A channel that cannot be made fails before the broker is asked.
    c.transport.create_pair_fail = Some(-12);
    let before = c.transport.requests.len();
    assert_eq!(c.attach_bell(id), Err(-12));
    assert_eq!(c.transport.requests.len(), before);
}

#[test]
fn bell_messages_are_deduplicated_and_unknown_ones_ignored() {
    let mut c = client();
    let a = subscribed(&mut c, Qos::Latest);
    let b = subscribed(&mut c, Qos::Latest);
    let mine_a = c.attach_bell(a).unwrap();
    c.attach_bell(b).unwrap();
    let queue = c.transport.bells.get_mut(&mine_a).unwrap();
    queue.push_back(ready(a.0));
    queue.push_back(ready(a.0));
    queue.push_back(ready(9999)); // not ours
    queue.push_back(vec![0xde, 0xad]); // not a parcel
    queue.push_back(ready(0)); // never a subscription
    let rung = c.consume_ready();
    assert_eq!(rung.ready, vec![a]);
}

#[test]
fn a_foreign_message_on_a_bell_is_not_a_ready() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    let mine = c.attach_bell(id).unwrap();
    // A well-formed parcel of another interface.
    let mut other = Parcel::default();
    other.header.interface_id = topics::INTERFACE_ID;
    other.header.method = bell::METHOD_READY;
    other.body = bell::encode_ready_args(&bell::ReadyArgs { subscription: id.0 }).unwrap();
    let mut bytes = Vec::new();
    other.encode(&mut bytes).unwrap();
    c.transport.bells.get_mut(&mine).unwrap().push_back(bytes);
    assert!(c.consume_ready().ready.is_empty());
}

#[test]
fn a_dead_bell_is_reported_and_closed() {
    let mut c = client();
    let a = subscribed(&mut c, Qos::Latest);
    let b = subscribed(&mut c, Qos::Latest);
    let bell_a = c.attach_bell(a).unwrap();
    let bell_b = c.attach_bell(b).unwrap();
    c.transport.bell_dead.push(bell_a);
    c.transport.publish(b.0, "a/x", b"1");
    let rung = c.consume_ready();
    assert_eq!(rung.ready, vec![b]);
    assert_eq!(rung.lost, vec![(a, -32)]);
    assert_eq!(c.doorbell_handles(), vec![bell_b]);
    assert_eq!(c.has_bell(a), Ok(false));
    assert!(c.transport.closed.contains(&bell_a));
    // The subscription itself lives on, drainable by explicit refresh.
    assert!(c.holds(a));
}

#[test]
fn a_bell_flood_is_bounded_per_call() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    let mine = c.attach_bell(id).unwrap();
    let queue = c.transport.bells.get_mut(&mine).unwrap();
    for _ in 0..(MAX_BELL_MESSAGES * 3) {
        queue.push_back(ready(id.0));
    }
    assert_eq!(c.consume_ready().ready, vec![id]);
    let left = c.transport.bells[&mine].len();
    assert_eq!(left, MAX_BELL_MESSAGES * 2);
}

// -- Lifecycle ---------------------------------------------------------------

#[test]
fn ids_are_tracked_from_subscribe_to_unsubscribe() {
    let mut c = client();
    let a = subscribed(&mut c, Qos::Latest);
    let b = subscribed(&mut c, Qos::Conflate);
    assert_ne!(a, b);
    assert_eq!(c.ids(), vec![a, b]);
    let bell_a = c.attach_bell(a).unwrap();

    assert_eq!(c.unsubscribe(a), Ok(()));
    assert!(!c.holds(a) && c.holds(b));
    assert!(c.transport.closed.contains(&bell_a));
    assert!(c.doorbell_handles().is_empty());
    assert_eq!(c.unsubscribe(a), Err(-2), "a second unsubscribe");
    assert_eq!(c.drain(a, 1).error, Some(-2));
    assert!(!c.transport.subs.contains_key(&a.0));
}

#[test]
fn a_failed_unsubscribe_keeps_the_record_so_the_caller_can_retry() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    let bell = c.attach_bell(id).unwrap();
    c.transport
        .transport_fail
        .push_back((topics::METHOD_UNSUBSCRIBE, -110));
    assert_eq!(c.unsubscribe(id), Err(-110));
    assert!(c.holds(id), "the id is still ours: the broker still has the queue");
    assert_eq!(c.doorbell_handles().len(), 1, "and its doorbell");
    assert!(!c.transport.closed.contains(&bell));
    assert!(c.transport.subs.contains_key(&id.0));
    // A broker error field fails it the same way.
    c.transport.fail.push_back((topics::METHOD_UNSUBSCRIBE, 13));
    assert_eq!(c.unsubscribe(id), Err(-13));
    assert!(c.holds(id));
    // The retry goes through and then the id is gone everywhere.
    assert_eq!(c.unsubscribe(id), Ok(()));
    assert!(!c.holds(id));
    assert!(c.doorbell_handles().is_empty());
    assert!(c.transport.closed.contains(&bell));
    assert!(!c.transport.subs.contains_key(&id.0));
}

#[test]
fn a_subscription_the_broker_forgot_is_dropped_locally_as_a_success() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    c.attach_bell(id).unwrap();
    // The broker restarted: it answers "no such subscription".
    c.transport.subs.remove(&id.0);
    assert_eq!(c.unsubscribe(id), Ok(()));
    assert!(!c.holds(id));
    assert!(c.doorbell_handles().is_empty());
}

#[test]
fn the_drop_after_a_failed_unsubscribe_tries_once_more() {
    let shared = Shared(Rc::new(RefCell::new(Fake::new())));
    let fake = shared.0.clone();
    let mut c = Subscriber::with_transport(shared);
    let id = c.subscribe("a/#", Qos::Latest, 1).unwrap();
    fake.borrow_mut()
        .transport_fail
        .push_back((topics::METHOD_UNSUBSCRIBE, -110));
    assert_eq!(c.unsubscribe(id), Err(-110));
    drop(c);
    assert!(fake.borrow().subs.is_empty(), "the last attempt succeeded");
}

#[test]
fn a_repeated_id_from_the_broker_is_refused() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    c.transport.next_id_for_test(id.0);
    assert_eq!(c.subscribe("b", Qos::Latest, 1), Err(-22));
    assert_eq!(c.ids(), vec![id]);
}

#[test]
fn reset_closes_doorbells_without_asking_the_broker() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    let bell_handle = c.attach_bell(id).unwrap();
    let calls = c.transport.requests.len();
    c.reset();
    assert!(c.ids().is_empty());
    assert_eq!(c.transport.requests.len(), calls);
    assert_eq!(c.transport.closed, vec![bell_handle]);
}

#[test]
fn dropping_the_subscriber_unsubscribes_without_blocking() {
    let shared = Shared(Rc::new(RefCell::new(Fake::new())));
    let fake = shared.0.clone();
    let mut c = Subscriber::with_transport(shared);
    let id = c.subscribe("a/#", Qos::Latest, 1).unwrap();
    let bell_handle = c.attach_bell(id).unwrap();
    drop(c);
    let fake = fake.borrow();
    assert!(fake.subs.is_empty(), "the broker forgot the subscription");
    assert!(fake.closed.contains(&bell_handle));
    assert_eq!(
        fake.log.last(),
        Some(&(topics::METHOD_UNSUBSCRIBE, Wait::Poll))
    );
}
