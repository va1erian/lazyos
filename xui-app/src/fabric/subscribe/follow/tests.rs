//! The hub's core against the scripted broker: subscriptions, doorbells,
//! queues, reconnects.

use std::rc::Rc;

use messenger_generated::os_lazy_messenger_topics_v1 as topics;

use super::super::test_fake::Fake;
use super::core::Core;
use super::{Queue, TopicEvent, QUEUE_CAP};
use crate::fabric::{Qos, Subscriber};

fn core() -> Core<Fake> {
    Core::new(Subscriber::with_transport(Fake::new()))
}

fn add(core: &mut Core<Fake>, key: u64, filter: &str) -> Rc<Queue> {
    let queue = Rc::new(Queue::default());
    core.add(key, filter, Qos::Buffered, 8, Rc::clone(&queue));
    queue
}

fn id_of(core: &Core<Fake>) -> u64 {
    *core
        .subscriber
        .transport
        .subs
        .keys()
        .next()
        .expect("one subscription")
}

fn event(topic: &str, sequence: u64, retained: bool) -> topics::Event {
    topics::Event {
        topic: topic.into(),
        publisher: 3,
        sequence,
        retained,
        payload: b"r".to_vec(),
    }
}

#[test]
fn a_feed_subscribes_with_a_doorbell_and_gets_what_is_published() {
    let mut core = core();
    let queue = add(&mut core, 1, "a/#");
    assert_eq!(
        core.bell_of(1),
        None,
        "nothing is subscribed before connect"
    );
    core.connect(0);
    assert!(core.bell_of(1).is_some(), "a doorbell to park on");
    assert!(queue.subscribed.get());
    assert_eq!(core.retry_at(), None);

    core.subscriber.transport.publish(id_of(&core), "a/1", b"x");
    core.service(0);
    let events = queue.take();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].topic, "a/1");
    assert_eq!(events[0].payload, b"x");
    assert_eq!(events[0].publisher, 7);
    assert!(!events[0].retained);

    // Read once, and nothing more until the broker rings again.
    core.service(1);
    assert!(queue.take().is_empty());
}

#[test]
fn a_new_subscription_is_read_without_a_ring() {
    let mut core = core();
    let queue = add(&mut core, 1, "a/#");
    core.connect(0);
    // The broker queued a replay before the doorbell existed: no bell message
    // is waiting, yet the first service reads it.
    let id = id_of(&core);
    let sub = core.subscriber.transport.subs.get_mut(&id).unwrap();
    sub.queue.push_back(event("a/old", 1, true));
    core.service(0);
    let events = queue.take();
    assert_eq!(events.len(), 1);
    assert!(events[0].retained);
}

#[test]
fn a_refused_subscription_is_tried_again_after_the_retry_time() {
    let mut core = core();
    let queue = add(&mut core, 1, "a/#");
    core.subscriber
        .transport
        .transport_fail
        .push_back((topics::METHOD_SUBSCRIBE, -2));
    core.connect(10);
    assert_eq!(core.bell_of(1), None);
    assert!(!queue.subscribed.get());
    let retry = core.retry_at().expect("a retry is due");
    assert!(retry > 10);

    core.connect(retry - 1);
    assert_eq!(core.bell_of(1), None, "too early");
    core.connect(retry);
    assert!(core.bell_of(1).is_some());
    core.subscriber.transport.publish(id_of(&core), "a/1", b"y");
    core.service(retry);
    assert_eq!(queue.take().len(), 1);
}

#[test]
fn a_lost_doorbell_resubscribes_and_the_feed_goes_on() {
    let mut core = core();
    let queue = add(&mut core, 1, "a/#");
    core.connect(0);
    let bell = core.bell_of(1).expect("a bell");
    core.subscriber.transport.bell_dead.push(bell);
    core.service(5);
    assert_eq!(
        core.retry_at(),
        Some(205),
        "made again after the retry time"
    );
    assert!(!queue.subscribed.get());

    core.connect(205);
    assert!(core.bell_of(1).is_some());
    core.subscriber.transport.publish(id_of(&core), "a/2", b"z");
    core.service(205);
    assert_eq!(queue.take()[0].topic, "a/2");
}

#[test]
fn removing_a_feed_drops_its_subscription() {
    let mut core = core();
    add(&mut core, 1, "a/#");
    add(&mut core, 2, "b/#");
    core.connect(0);
    assert_eq!(core.subscriber.transport.subs.len(), 2);
    core.remove(1);
    assert_eq!(core.subscriber.transport.subs.len(), 1);
    core.remove(2);
    assert!(core.subscriber.transport.subs.is_empty());
    assert_eq!(core.bell_of(2), None);
}

#[test]
fn a_flood_stops_at_the_batch_and_the_next_service_goes_on() {
    let mut core = core();
    let queue = add(&mut core, 1, "a/#");
    core.connect(0);
    let id = id_of(&core);
    core.subscriber
        .transport
        .flood(id, super::super::MAX_DRAIN + 10);
    core.service(0);
    assert!(core.busy(), "the bell is not re-armed: read again");
    assert_eq!(queue.take().len(), super::super::MAX_DRAIN);
    core.service(0);
    assert!(!core.busy());
    assert_eq!(queue.take().len(), 10);
}

#[test]
fn a_ring_that_finds_nothing_is_read_again() {
    let mut core = core();
    let queue = add(&mut core, 1, "a/#");
    core.connect(0);
    core.service(0);
    let id = id_of(&core);
    // The broker rang, but the poll missed the event (it answered nothing).
    core.subscriber.transport.publish(id, "a/1", b"x");
    core.subscriber
        .transport
        .subs
        .get_mut(&id)
        .unwrap()
        .queue
        .clear();
    core.service(1);
    assert!(queue.take().is_empty());
    // The event is there on the next look: the empty ring left the feed
    // pending, so it is read without another ring.
    let sub = core.subscriber.transport.subs.get_mut(&id).unwrap();
    sub.queue.push_back(event("a/2", 9, false));
    core.service(2);
    assert_eq!(queue.take()[0].topic, "a/2");
}

#[test]
fn a_queue_nobody_reads_keeps_the_newest() {
    let queue = Queue::default();
    let events = |from: usize, to: usize| {
        (from..to)
            .map(|n| TopicEvent {
                topic: format!("t/{n}"),
                retained: false,
                publisher: 0,
                payload: Vec::new(),
            })
            .collect::<Vec<_>>()
    };
    queue.push(events(0, QUEUE_CAP + 5));
    let kept = queue.take();
    assert_eq!(kept.len(), QUEUE_CAP);
    assert_eq!(kept[0].topic, "t/5");
    assert_eq!(kept.last().unwrap().topic, format!("t/{}", QUEUE_CAP + 4));
}

#[test]
fn a_permanent_refusal_is_not_asked_again() {
    let mut core = core();
    add(&mut core, 1, "a/#");
    core.subscriber
        .transport
        .transport_fail
        .push_back((topics::METHOD_SUBSCRIBE, -13));
    core.connect(0);
    let asked = core.subscriber.transport.log.len();
    assert_eq!(core.retry_at(), None, "nothing is waiting for a retry");
    core.connect(1_000_000);
    assert_eq!(core.subscriber.transport.log.len(), asked, "no second ask");
    assert_eq!(core.bell_of(1), None);
}

#[test]
fn a_refusal_is_permanent_but_a_reply_that_did_not_parse_is_not() {
    let mut core = core();
    add(&mut core, 1, "a/#");
    add(&mut core, 2, "b/#");
    core.subscriber.transport.garbage = Some(vec![0xff; 40]);
    core.connect(0);
    assert!(
        core.retry_at().is_some(),
        "a malformed answer is asked about again"
    );
    core.subscriber.transport.garbage = None;
    core.connect(1_000_000);
    assert!(core.bell_of(1).is_some() && core.bell_of(2).is_some());

    let mut core = self::core();
    add(&mut core, 1, "a/#");
    core.subscriber
        .transport
        .transport_fail
        .push_back((topics::METHOD_SUBSCRIBE, -22));
    core.connect(0);
    assert_eq!(
        core.retry_at(),
        None,
        "the broker said invalid: never again"
    );
}

#[test]
fn a_lost_feed_is_dropped_from_the_subscriber_whatever_the_broker_says() {
    let mut core = core();
    add(&mut core, 1, "a/#");
    core.connect(0);
    let id = crate::fabric::SubId(id_of(&core));
    let bell = core.bell_of(1).expect("a bell");
    core.subscriber.transport.bell_dead.push(bell);
    core.subscriber
        .transport
        .transport_fail
        .push_back((topics::METHOD_UNSUBSCRIBE, -110));
    core.service(5);
    assert!(!core.subscriber.holds(id), "no stale record left to read");
    assert!(core.bells().is_empty(), "and no stale bell to park on");
}

#[test]
fn the_hub_says_when_it_owes_a_read() {
    let mut core = core();
    add(&mut core, 1, "a/#");
    assert!(!core.owes());
    core.connect(0);
    assert!(core.owes(), "a new subscription's replay is owed");
    core.service(0);
    core.service(1);
    core.service(2);
    assert!(!core.owes(), "settled once the looks are used up");
    assert_eq!(core.bells().len(), 1);
}
