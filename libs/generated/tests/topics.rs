//! Round-trip tests for the generated topics interfaces (issue #299):
//! `os.lazy.messenger.topics.v1` and its publish/subscribe ACL scopes.

use libmessenger::Encoder;
use messenger_generated::os_lazy_messenger_topics_publish_v1 as publish_scope;
use messenger_generated::os_lazy_messenger_topics_subscribe_v1 as subscribe_scope;
use messenger_generated::os_lazy_messenger_topics_v1::*;

/// FNV-1a 64, the interface-id hash (mirrors `tools/midlc`).
fn fnv1a64(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[test]
fn ids_match_the_documented_names_and_legacy_constants() {
    assert_eq!(INTERFACE_ID, fnv1a64("os.lazy.messenger.topics.v1"));
    assert_eq!(INTERFACE_ID, 0xc573_4f97_8fef_7231);
    assert_eq!(
        publish_scope::INTERFACE_ID,
        fnv1a64("os.lazy.messenger.topics.publish.v1")
    );
    assert_eq!(
        subscribe_scope::INTERFACE_ID,
        fnv1a64("os.lazy.messenger.topics.subscribe.v1")
    );
    // The scopes must be distinct or policy could not tell the modes apart.
    assert_ne!(publish_scope::INTERFACE_ID, subscribe_scope::INTERFACE_ID);
    assert_ne!(publish_scope::INTERFACE_ID, INTERFACE_ID);
    // Method ids that predate the IDL are unchanged.
    assert_eq!(METHOD_PUBLISH, 1_818_372_520);
    assert_eq!(METHOD_SUBSCRIBE, 6_992_035);
    assert_eq!(METHOD_UNSUBSCRIBE, 2_099_666_486);
    assert_eq!(METHOD_NEXTEVENT, 1_278_354_512);
    assert_eq!(METHOD_ACK, 483_717_538);
    assert_eq!(METHOD_LISTTOPICS, 225_427_937);
    assert_eq!(METHOD_PING, 2_142_761_129);
}

#[test]
fn qos_and_mode_constants_are_the_enum_indices() {
    assert_eq!(
        [QOS_LATEST, QOS_BUFFERED, QOS_CONFLATE, QOS_RELIABLE],
        [0, 1, 2, 3]
    );
    assert_eq!(publish_scope::MODE_PUBLISH, 0);
    assert_eq!(publish_scope::MODE_SUBSCRIBE, 1);
    assert_eq!(subscribe_scope::MODE_PUBLISH, 0);
    assert_eq!(subscribe_scope::MODE_SUBSCRIBE, 1);
}

#[test]
fn publish_roundtrips_empty_and_large_payloads() {
    for payload in [vec![], vec![0u8, 255, 7], vec![0xa5u8; 8 * 1024]] {
        for retained in [false, true] {
            let args = PublishArgs {
                topic: "system/events/network/up".into(),
                payload: payload.clone(),
                retained,
            };
            assert_eq!(
                decode_publish_args(&encode_publish_args(&args).unwrap()).unwrap(),
                args
            );
        }
    }
    let reply = PublishReply { matched: u64::MAX };
    assert_eq!(
        decode_publish_reply(&encode_publish_reply(&reply).unwrap()).unwrap(),
        reply
    );
}

#[test]
fn subscribe_roundtrips_every_qos_variant() {
    for (qos, depth) in [
        (QOS_LATEST, 1),
        (QOS_BUFFERED, 64),
        (QOS_BUFFERED, 0),
        (QOS_CONFLATE, 4),
        (QOS_RELIABLE, 8),
    ] {
        let args = SubscribeArgs {
            filter: "system/+/up".into(),
            qos,
            depth,
        };
        assert_eq!(
            decode_subscribe_args(&encode_subscribe_args(&args).unwrap()).unwrap(),
            args
        );
    }
    let reply = SubscribeReply { subscription: 7 };
    assert_eq!(
        decode_subscribe_reply(&encode_subscribe_reply(&reply).unwrap()).unwrap(),
        reply
    );
}

#[test]
fn subscription_addressed_requests_share_one_layout() {
    // The broker decodes NextEvent/Stats requests with the Unsubscribe
    // decoder; pin that the three are byte-identical.
    let unsub = encode_unsubscribe_args(&UnsubscribeArgs { subscription: 42 }).unwrap();
    let next = encode_next_event_args(&NextEventArgs { subscription: 42 }).unwrap();
    let stats = encode_stats_args(&StatsArgs { subscription: 42 }).unwrap();
    assert_eq!(unsub, next);
    assert_eq!(unsub, stats);
    assert_eq!(decode_unsubscribe_args(&next).unwrap().subscription, 42);

    let ack = AckArgs {
        subscription: 9,
        sequence: 1234,
    };
    assert_eq!(
        decode_ack_args(&encode_ack_args(&ack).unwrap()).unwrap(),
        ack
    );
}

#[test]
fn event_reply_roundtrips_including_empty_and_large_payload() {
    for payload in [vec![], vec![1u8, 2, 3], vec![0x5au8; 8 * 1024]] {
        let reply = NextEventReply {
            event: Event {
                topic: "session/1/clipboard/changed".into(),
                publisher: 3,
                sequence: 99,
                retained: true,
                payload,
            },
        };
        assert_eq!(
            decode_next_event_reply(&encode_next_event_reply(&reply).unwrap()).unwrap(),
            reply
        );
    }
}

#[test]
fn topic_list_and_stats_roundtrip() {
    for topics in [
        vec![],
        vec![TopicInfo {
            topic: "a".into(),
            subscribers: 0,
            retained: false,
        }],
        (0..40)
            .map(|index| TopicInfo {
                topic: format!("system/events/t{index}"),
                subscribers: index,
                retained: index % 2 == 0,
            })
            .collect(),
    ] {
        let reply = ListTopicsReply { topics };
        assert_eq!(
            decode_list_topics_reply(&encode_list_topics_reply(&reply).unwrap()).unwrap(),
            reply
        );
    }
    let reply = StatsReply {
        stats: Stats {
            qos: QOS_RELIABLE,
            depth: 8,
            queued: 3,
            delivered: 10,
            matched: 12,
            drops: 2,
        },
    };
    assert_eq!(
        decode_stats_reply(&encode_stats_reply(&reply).unwrap()).unwrap(),
        reply
    );
}

#[test]
fn authorize_request_roundtrips_in_both_scopes() {
    let args = publish_scope::AuthorizeTopicArgs {
        name: "system/events".into(),
        mode: publish_scope::MODE_PUBLISH,
        txn: 0xabcdef,
    };
    let body = publish_scope::encode_authorize_topic_args(&args).unwrap();
    assert_eq!(
        publish_scope::decode_authorize_topic_args(&body).unwrap(),
        args
    );
    // One layout serves both scopes: the kernel decodes either with one codec.
    let sub = subscribe_scope::AuthorizeTopicArgs {
        name: "system/#".into(),
        mode: subscribe_scope::MODE_SUBSCRIBE,
        txn: 0,
    };
    let sub_body = subscribe_scope::encode_authorize_topic_args(&sub).unwrap();
    let via_publish = publish_scope::decode_authorize_topic_args(&sub_body).unwrap();
    assert_eq!(via_publish.name, sub.name);
    assert_eq!(via_publish.mode, sub.mode);
    assert_eq!(
        publish_scope::METHOD_AUTHORIZETOPIC,
        subscribe_scope::METHOD_AUTHORIZETOPIC
    );
}

#[test]
fn truncated_bodies_are_rejected() {
    let body = encode_publish_args(&PublishArgs {
        topic: "a/b".into(),
        payload: vec![9u8; 64],
        retained: true,
    })
    .unwrap();
    assert!(decode_publish_args(&body[..body.len() - 5]).is_err());
    let body = encode_next_event_reply(&NextEventReply {
        event: Event {
            topic: "a/b".into(),
            payload: vec![1, 2, 3],
            ..Event::default()
        },
    })
    .unwrap();
    assert!(decode_next_event_reply(&body[..body.len() - 3]).is_err());
    let body = publish_scope::encode_authorize_topic_args(&publish_scope::AuthorizeTopicArgs {
        name: "system/events".into(),
        mode: 0,
        txn: 1,
    })
    .unwrap();
    assert!(publish_scope::decode_authorize_topic_args(&body[..body.len() - 2]).is_err());
}

#[test]
fn unknown_fields_are_ignored() {
    let mut body = encode_subscribe_args(&SubscribeArgs {
        filter: "a/#".into(),
        qos: QOS_CONFLATE,
        depth: 4,
    })
    .unwrap();
    let mut extra = Encoder::new();
    extra.u64(30, 0xdead).unwrap();
    body.extend_from_slice(&extra.finish());
    let decoded = decode_subscribe_args(&body).unwrap();
    assert_eq!(decoded.filter, "a/#");
    assert_eq!(decoded.qos, QOS_CONFLATE);
    assert_eq!(decoded.depth, 4);
}

#[test]
fn missing_fields_decode_to_defaults() {
    assert_eq!(
        decode_subscribe_args(&[]).unwrap(),
        SubscribeArgs::default()
    );
    assert_eq!(decode_publish_reply(&[]).unwrap().matched, 0);
}
