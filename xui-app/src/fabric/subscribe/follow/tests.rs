//! The watcher's core against the scripted broker: subscriptions, doorbells,
//! queues, reconnects.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use messenger_generated::os_lazy_messenger_topics_v1 as topics;

use super::super::fake::Fake;
use super::core::{Command, Core};
use super::{Queue, TopicEvent, Wake, QUEUE_CAP};
use crate::fabric::{Qos, Subscriber};

/// A feed's pieces for a test: the queue and how often its wake rang.
fn add(core: &mut Core<Fake>, key: u64, filter: &str) -> (Arc<Queue>, Arc<AtomicUsize>) {
    let queue = Arc::new(Queue::default());
    let rang = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&rang);
    let wake: Wake = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    core.apply(Command::Add {
        key,
        filter: filter.to_owned(),
        qos: Qos::Buffered,
        depth: 8,
        queue: Arc::clone(&queue),
        wake,
    });
    (queue, rang)
}

fn core() -> Core<Fake> {
    Core::new(Subscriber::with_transport(Fake::new()))
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

fn drained(queue: &Queue) -> Vec<TopicEvent> {
    queue.take()
}

#[test]
fn a_feed_subscribes_with_a_doorbell_and_gets_what_is_published() {
    let mut core = core();
    let (queue, rang) = add(&mut core, 1, "a/#");
    assert!(
        core.handles().is_empty(),
        "nothing is subscribed before connect"
    );
    core.connect(0);
    assert_eq!(core.handles().len(), 1, "one doorbell to park on");
    assert_eq!(core.retry_at(), None);

    core.subscriber.transport.publish(id_of(&core), "a/1", b"x");
    let wakes = core.service(0);
    assert_eq!(wakes.len(), 1);
    wakes[0]();
    assert_eq!(rang.load(Ordering::SeqCst), 1);
    let events = drained(&queue);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].topic, "a/1");
    assert_eq!(events[0].payload, b"x");
    assert!(!events[0].retained);

    // Read once, and nothing more until the broker rings again.
    assert!(core.service(1).is_empty());
    assert!(drained(&queue).is_empty());
}

#[test]
fn a_new_subscription_is_read_without_a_ring() {
    let mut core = core();
    let (queue, _) = add(&mut core, 1, "a/#");
    core.connect(0);
    // The broker queued a replay before the doorbell existed: no bell message
    // is waiting, yet the first service reads it.
    let id = id_of(&core);
    core.subscriber
        .transport
        .subs
        .get_mut(&id)
        .unwrap()
        .queue
        .push_back(topics::Event {
            topic: "a/old".into(),
            publisher: 3,
            sequence: 1,
            retained: true,
            payload: b"r".to_vec(),
        });
    assert_eq!(core.service(0).len(), 1);
    let events = drained(&queue);
    assert_eq!(events.len(), 1);
    assert!(events[0].retained);
}

#[test]
fn a_refused_subscription_is_tried_again_after_the_retry_time() {
    let mut core = core();
    let (queue, _) = add(&mut core, 1, "a/#");
    core.subscriber
        .transport
        .transport_fail
        .push_back((topics::METHOD_SUBSCRIBE, -2));
    core.connect(10);
    assert!(core.handles().is_empty());
    let retry = core.retry_at().expect("a retry is due");
    assert!(retry > 10);

    core.connect(retry - 1);
    assert!(core.handles().is_empty(), "too early");
    core.connect(retry);
    assert_eq!(core.handles().len(), 1);
    assert_eq!(core.retry_at(), None);
    core.subscriber.transport.publish(id_of(&core), "a/1", b"y");
    core.service(retry);
    assert_eq!(drained(&queue).len(), 1);
}

#[test]
fn a_lost_doorbell_resubscribes_and_the_feed_goes_on() {
    let mut core = core();
    let (queue, _) = add(&mut core, 1, "a/#");
    core.connect(0);
    let bell = core.handles()[0];
    core.subscriber.transport.bell_dead.push(bell);
    core.service(5);
    assert_eq!(
        core.retry_at(),
        Some(105),
        "made again after the retry time"
    );

    core.connect(105);
    assert_eq!(core.handles().len(), 1);
    core.subscriber.transport.publish(id_of(&core), "a/2", b"z");
    core.service(105);
    assert_eq!(drained(&queue)[0].topic, "a/2");
}

#[test]
fn removing_a_feed_drops_its_subscription() {
    let mut core = core();
    let (_, _) = add(&mut core, 1, "a/#");
    let (_, _) = add(&mut core, 2, "b/#");
    core.connect(0);
    assert_eq!(core.subscriber.transport.subs.len(), 2);
    core.apply(Command::Remove(1));
    assert_eq!(core.subscriber.transport.subs.len(), 1);
    core.apply(Command::Remove(2));
    assert!(core.subscriber.transport.subs.is_empty());
    assert!(core.handles().is_empty());
}

#[test]
fn a_flood_stops_at_the_batch_and_the_next_service_goes_on() {
    let mut core = core();
    let (queue, _) = add(&mut core, 1, "a/#");
    core.connect(0);
    let id = id_of(&core);
    core.subscriber
        .transport
        .flood(id, super::super::MAX_DRAIN + 10);
    core.service(0);
    assert!(core.busy(), "the bell is not re-armed: do not park");
    assert_eq!(drained(&queue).len(), super::super::MAX_DRAIN);
    core.service(0);
    assert!(!core.busy());
    assert_eq!(drained(&queue).len(), 10);
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
