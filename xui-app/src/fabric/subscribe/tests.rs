//! The subscriber against a scripted broker: wire goldens, errors and the
//! drain loop (the doorbell and lifecycle are in `lifecycle_tests`).

use libmessenger::{flags, Parcel};
use messenger_generated::os_lazy_messenger_topics_v1 as topics;

use super::fake::{EmptyAs, Fake};
use super::{wire, Qos, SubId, Subscriber, Wait, ETIMEDOUT, MAX_DRAIN};

pub(super) type Client = Subscriber<Fake>;

pub(super) fn client() -> Client {
    Subscriber::with_transport(Fake::new())
}

pub(super) fn subscribed(client: &mut Client, qos: Qos) -> SubId {
    client.subscribe("a/#", qos, 8).expect("subscribe")
}

fn decoded(request: &[u8]) -> Parcel {
    Parcel::decode(request).expect("well-formed request")
}

// -- Wire goldens ------------------------------------------------------------

#[test]
fn requests_match_the_generated_wire_functions() {
    let mut c = client();
    let id = c.subscribe("system/+/up", Qos::Buffered, 12).unwrap();
    let handle = c.attach_bell(id).unwrap();
    c.stats(id).unwrap();
    c.ack(id, 77).unwrap();
    c.drain(id, 1);
    c.unsubscribe(id).unwrap();
    let requests: Vec<Parcel> = c.transport.requests.iter().map(|r| decoded(r)).collect();

    let methods: Vec<u32> = requests.iter().map(|p| p.header.method).collect();
    assert_eq!(
        methods,
        [
            topics::METHOD_SUBSCRIBE,
            topics::METHOD_BELL,
            topics::METHOD_STATS,
            topics::METHOD_ACK,
            topics::METHOD_NEXTEVENT,
            topics::METHOD_UNSUBSCRIBE,
        ]
    );
    for parcel in &requests {
        assert_eq!(parcel.header.interface_id, topics::INTERFACE_ID);
        assert_eq!(parcel.header.flags, flags::SYNC | flags::ALLOW_NESTED);
        assert_eq!(parcel.header.version, libmessenger::VERSION);
    }
    let subscribe = topics::SubscribeArgs {
        filter: "system/+/up".into(),
        qos: topics::QOS_BUFFERED,
        depth: 12,
    };
    assert_eq!(
        requests[0].body,
        topics::encode_subscribe_args(&subscribe).unwrap()
    );
    // The bell carries exactly one channel object and the id.
    assert_eq!(requests[1].objects.len(), 1);
    assert!(matches!(
        requests[1].objects[0],
        libmessenger::Object::Channel(_)
    ));
    let bell_args = topics::BellArgs {
        subscription: id.0,
        bell: requests[1].objects[0].handle(),
    };
    assert_eq!(
        requests[1].body,
        topics::encode_bell_args(&bell_args).unwrap().0
    );
    // The handle in the request is the end the client did not keep.
    assert_ne!(requests[1].objects[0].handle(), handle);
    let by_id =
        topics::encode_unsubscribe_args(&topics::UnsubscribeArgs { subscription: id.0 }).unwrap();
    assert_eq!(requests[2].body, by_id);
    assert_eq!(requests[4].body, by_id);
    assert_eq!(requests[5].body, by_id);
    let ack = topics::AckArgs {
        subscription: id.0,
        sequence: 77,
    };
    assert_eq!(requests[3].body, topics::encode_ack_args(&ack).unwrap());
}

#[test]
fn subscribe_depth_is_clamped_to_the_brokers_range() {
    let mut c = client();
    for (asked, sent) in [(0, 1), (1, 1), (64, 64), (500, 64)] {
        c.subscribe("x", Qos::Buffered, asked).unwrap();
        let last = decoded(c.transport.requests.last().unwrap());
        let args = topics::decode_subscribe_args(&last.body).unwrap();
        assert_eq!(args.depth, sent, "asked {asked}");
    }
    for (qos, code) in [
        (Qos::Latest, 0),
        (Qos::Buffered, 1),
        (Qos::Conflate, 2),
        (Qos::Reliable, 3),
    ] {
        assert_eq!(qos.code(), code);
    }
}

#[test]
fn polls_use_an_expired_deadline_and_calls_a_bounded_one() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    c.stats(id).unwrap();
    c.drain(id, 4);
    let log = &c.transport.log;
    assert_eq!(log[0], (topics::METHOD_SUBSCRIBE, Wait::Bounded));
    assert_eq!(log[1], (topics::METHOD_STATS, Wait::Bounded));
    assert_eq!(log[2], (topics::METHOD_NEXTEVENT, Wait::Poll));
}

// -- Error decoding ----------------------------------------------------------

#[test]
fn broker_error_fields_become_negative_errno() {
    let mut c = client();
    for code in [1u32, 2, 13, 22, 110] {
        c.transport.fail.push_back((topics::METHOD_SUBSCRIBE, code));
        assert_eq!(c.subscribe("a", Qos::Latest, 1), Err(-(code as i64)));
    }
    assert!(c.ids().is_empty());

    // A code of zero is still a failure.
    c.transport.fail.push_back((topics::METHOD_SUBSCRIBE, 0));
    assert_eq!(c.subscribe("a", Qos::Latest, 1), Err(-22));

    // Errors on every other call.
    let id = subscribed(&mut c, Qos::Reliable);
    c.transport.fail.push_back((topics::METHOD_STATS, 13));
    assert_eq!(c.stats(id), Err(-13));
    c.transport.fail.push_back((topics::METHOD_ACK, 1));
    assert_eq!(c.ack(id, 1), Err(-1));
    c.transport.fail.push_back((topics::METHOD_NEXTEVENT, 1));
    let drained = c.drain(id, 4);
    assert_eq!((drained.events.len(), drained.error), (0, Some(-1)));
}

#[test]
fn transport_failures_pass_through() {
    let mut c = client();
    c.transport
        .transport_fail
        .push_back((topics::METHOD_SUBSCRIBE, -ETIMEDOUT));
    assert_eq!(c.subscribe("a", Qos::Latest, 1), Err(-ETIMEDOUT));
    c.transport
        .transport_fail
        .push_back((topics::METHOD_SUBSCRIBE, -2));
    assert_eq!(c.subscribe("a", Qos::Latest, 1), Err(-2));
}

#[test]
fn reply_decoding_rejects_malformed_answers() {
    let error = |body: Vec<u8>| {
        let mut p = Parcel::default();
        p.body = body;
        p
    };
    // A subscription id of zero is the absent-field default.
    let zero = topics::encode_subscribe_reply(&topics::SubscribeReply { subscription: 0 });
    assert_eq!(wire::decode_subscription(&error(zero.unwrap())), Err(-22));
    // An event without a topic is not one.
    let nameless = topics::encode_next_event_reply(&topics::NextEventReply {
        event: topics::Event::default(),
    });
    assert_eq!(wire::decode_event(&error(nameless.unwrap())), Err(-22));
    // The empty body is "nothing".
    assert_eq!(wire::decode_event(&error(Vec::new())), Ok(None));
    // Not a parcel at all.
    assert_eq!(wire::parse_reply(&[1, 2, 3]).map(drop), Err(-22));
}

// -- Locally refused calls ---------------------------------------------------

#[test]
fn bad_arguments_never_reach_the_broker() {
    let mut c = client();
    assert_eq!(c.subscribe("", Qos::Latest, 1), Err(-22));
    assert_eq!(c.subscribe(&"x".repeat(513), Qos::Latest, 1), Err(-22));
    let ghost = SubId(99);
    assert_eq!(c.ack(ghost, 1), Err(-2));
    assert_eq!(c.stats(ghost).map(drop), Err(-2));
    assert_eq!(c.attach_bell(ghost), Err(-2));
    assert_eq!(c.unsubscribe(ghost), Err(-2));
    assert_eq!(c.drain(ghost, 4).error, Some(-2));
    assert_eq!(c.has_bell(ghost), Err(-2));
    assert!(c.transport.requests.is_empty());
}

// -- The drain loop ----------------------------------------------------------

#[test]
fn draining_an_empty_queue_is_not_an_error() {
    for empty in [EmptyAs::Timeout, EmptyAs::EmptyBody] {
        let mut c = client();
        c.transport.empty_as = Some(empty);
        let id = subscribed(&mut c, Qos::Buffered);
        let drained = c.drain(id, 10);
        assert!(drained.events.is_empty(), "{empty:?}");
        assert!(!drained.more && drained.error.is_none(), "{empty:?}");
        assert_eq!(
            c.transport.log.last(),
            Some(&(topics::METHOD_NEXTEVENT, Wait::Poll))
        );
    }
}

#[test]
fn draining_returns_events_in_order_and_ends_on_an_empty_poll() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Buffered);
    for n in 0..5u8 {
        c.transport.publish(id.0, &format!("a/{n}"), &[n; 4]);
    }
    let drained = c.drain(id, 100);
    assert_eq!(drained.error, None);
    assert!(!drained.more);
    let topics_seen: Vec<&str> = drained.events.iter().map(|e| e.topic.as_str()).collect();
    assert_eq!(topics_seen, ["a/0", "a/1", "a/2", "a/3", "a/4"]);
    assert_eq!(drained.events[2].payload, vec![2u8; 4]);
    assert_eq!(drained.events[2].publisher, 7);
    assert!(drained
        .events
        .windows(2)
        .all(|w| w[0].sequence < w[1].sequence));
    // Five events plus the one empty poll that re-arms the bell.
    let polls = c
        .transport
        .log
        .iter()
        .filter(|(m, _)| *m == topics::METHOD_NEXTEVENT)
        .count();
    assert_eq!(polls, 6);
}

#[test]
fn a_drain_is_capped_and_says_there_is_more() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Buffered);
    c.transport.flood(id.0, MAX_DRAIN + 44);

    // Asking for less than the hard cap.
    let first = c.drain(id, 10);
    assert_eq!((first.events.len(), first.more), (10, true));
    // Asking for far more than the hard cap still stops at it.
    let second = c.drain(id, usize::MAX);
    assert_eq!((second.events.len(), second.more), (MAX_DRAIN, true));
    assert_eq!(second.events[0].sequence, first.events[9].sequence + 1);
    // The rest, and now the broker said "nothing".
    let third = c.drain(id, usize::MAX);
    assert_eq!((third.events.len(), third.more), (44 - 10, false));
    // `max == 0` still takes one (a drain that took nothing never re-arms).
    c.transport.flood(id.0, 3);
    assert_eq!(c.drain(id, 0).events.len(), 1);
}

#[test]
fn an_error_mid_drain_keeps_the_events_already_taken() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Buffered);
    c.transport.flood(id.0, 10);
    c.transport.poll_error_after = Some((3, -32));
    let drained = c.drain(id, 100);
    assert_eq!(drained.events.len(), 3);
    assert_eq!(drained.error, Some(-32));
    assert!(!drained.more);
    // The client is still usable; the rest of the queue is intact.
    let rest = c.drain(id, 100);
    assert_eq!((rest.events.len(), rest.error), (7, None));
}

#[test]
fn garbage_replies_end_a_drain_with_einval() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    c.transport.garbage = Some(vec![0xff; 40]);
    let drained = c.drain(id, 5);
    assert_eq!((drained.events.len(), drained.error), (0, Some(-22)));
    c.transport.garbage = None;
    assert_eq!(c.drain(id, 5).error, None);
}

#[test]
fn an_oversized_reply_is_refused_not_truncated() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Latest);
    c.transport.publish(id.0, "a/big", &vec![1u8; 20 * 1024]);
    let drained = c.drain(id, 5);
    assert_eq!((drained.events.len(), drained.error), (0, Some(-22)));
}

#[test]
fn a_reliable_drain_acks_each_event_and_reaches_the_end() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Reliable);
    for n in 0..5u8 {
        c.transport.publish(id.0, "a/r", &[n]);
    }
    let drained = c.drain(id, 100);
    assert_eq!((drained.events.len(), drained.error), (5, None));
    assert!(c.transport.subs[&id.0].queue.is_empty());
    let acks = c
        .transport
        .log
        .iter()
        .filter(|(m, _)| *m == topics::METHOD_ACK)
        .count();
    assert_eq!(acks, 5);
    // Without the acks the broker would hand out the head forever.
    assert_eq!(c.drain(id, 100).events.len(), 0);
}

#[test]
fn a_failed_ack_stops_the_drain_and_the_event_comes_again() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Reliable);
    c.transport.publish(id.0, "a/r", b"one");
    c.transport.publish(id.0, "a/r", b"two");
    c.transport
        .transport_fail
        .push_back((topics::METHOD_ACK, -110));
    let drained = c.drain(id, 100);
    assert_eq!(drained.events.len(), 1);
    assert_eq!(drained.error, Some(-110));
    let again = c.drain(id, 100);
    assert_eq!(again.events.len(), 2, "unacked head is redelivered");
    assert_eq!(again.events[0].sequence, drained.events[0].sequence);
}

#[test]
fn reliable_delivery_is_at_least_once_a_failed_ack_delivers_the_event_twice() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Reliable);
    c.transport.publish(id.0, "a/r", b"one");
    c.transport
        .transport_fail
        .push_back((topics::METHOD_ACK, -110));
    // The event is in the first drain's result AND still at the broker.
    let first = c.drain(id, 100);
    assert_eq!((first.events.len(), first.error), (1, Some(-110)));
    assert_eq!(
        c.transport.subs[&id.0].queue.len(),
        1,
        "not acked: still queued"
    );
    let second = c.drain(id, 100);
    assert_eq!((second.events.len(), second.error), (1, None));
    assert_eq!(second.events[0], first.events[0], "the same event, again");
    assert!(c.transport.subs[&id.0].queue.is_empty(), "acked this time");
    // From then on it is not delivered a third time.
    assert!(c.drain(id, 100).events.is_empty());
}

#[test]
fn stats_reports_the_brokers_counters() {
    let mut c = client();
    let id = subscribed(&mut c, Qos::Buffered);
    c.transport.flood(id.0, 4);
    c.drain(id, 1);
    let stats = c.stats(id).unwrap();
    assert_eq!(
        (stats.qos, stats.depth, stats.queued, stats.delivered),
        (topics::QOS_BUFFERED, 8, 3, 1)
    );
    assert_eq!((stats.matched, stats.drops), (9, 3));
}

#[test]
fn raw_event_payload_can_be_unwrapped() {
    let wrapped = libmessenger::envelope::wrap(b"hello").unwrap();
    let event = super::RawEvent {
        topic: "t".into(),
        publisher: 1,
        sequence: 1,
        retained: false,
        payload: wrapped,
    };
    assert_eq!(event.unwrapped_payload(), b"hello");
}
