use rhai::Map;
use rhai_lazy::msg::{schema, topics};

use super::tick_payload;
use crate::listener_decode::{decode, Body, MAX_DECODE, MAX_KEPT};
use crate::testkit::{cases, seed, Rng};

#[test]
fn a_declared_topic_decodes_by_its_type() {
    let body = decode("time/tick", &tick_payload(1_700_000_000));
    let Body::Decoded { ty, value } = &body else {
        panic!("{body:?}")
    };
    assert_eq!(ty, "Tick");
    let map = value.clone().cast::<Map>();
    assert_eq!(map["unix"].as_int().unwrap(), 1_700_000_000);
    assert_eq!(
        map["zone_name"].clone().into_string().unwrap(),
        "Europe/Paris"
    );
    assert!(body.tree().contains("zone_name: \"Europe/Paris\""));
    assert_eq!(body.kind_text(), "Tick");
    assert_eq!(body.declared(), Some("Tick"));
}

#[test]
fn an_undeclared_topic_is_bytes_and_a_bad_payload_says_so() {
    let wrapped = topics::wrap(b"hello").unwrap();
    match decode("demo/pg/raw", &wrapped) {
        Body::Bytes { bytes, len } => {
            assert_eq!(bytes, b"hello");
            assert_eq!(len, wrapped.len());
        }
        other => panic!("{other:?}"),
    }
    // A string where a Tick was declared.
    let body = decode("time/tick", &topics::wrap(b"not a tick").unwrap());
    match &body {
        Body::Undecodable { ty, error, .. } => {
            assert_eq!(ty, "Tick");
            assert!(!error.is_empty());
        }
        other => panic!("{other:?}"),
    }
    assert!(body.tree().starts_with("not a Tick: "));
    assert_eq!(body.kind_text(), "Tick?");
    assert!(!body.value().is_unit());
    // Too long to decode at all.
    let huge = vec![0u8; MAX_DECODE + 1];
    match decode("time/tick", &huge) {
        Body::Undecodable { error, bytes, .. } => {
            assert!(error.contains("too long"));
            assert_eq!(bytes.len(), MAX_KEPT);
        }
        other => panic!("{other:?}"),
    }
    match decode("demo/raw", &huge) {
        Body::Bytes { bytes, .. } => assert!(bytes.len() <= MAX_KEPT),
        other => panic!("{other:?}"),
    }
}

/// Topics whose payloads exercise structs, arrays, options and enums.
fn declared_samples() -> Vec<String> {
    let mut out = vec![
        "time/tick".to_string(),
        "system/stats/tasks".to_string(),
        "system/stats/memory".to_string(),
        "system/confd/changed/sys/x".to_string(),
    ];
    for interface in schema::interfaces() {
        for topic in interface.topics {
            out.push(topic.pattern.replace(['+', '#'], "x"));
        }
    }
    out
}

#[test]
fn decoding_arbitrary_payload_bytes_never_panics() {
    let topics_list = declared_samples();
    let mut rng = Rng(seed(0xdec0_de42));
    let mut decoded = 0u64;
    let mut refused = 0u64;
    for _ in 0..cases(30_000) {
        let topic = if rng.chance(5) {
            "demo/undeclared".to_string()
        } else {
            rng.pick(&topics_list).clone()
        };
        let len = rng.below(64) as usize;
        let mut payload = rng.bytes(len);
        if rng.chance(2) {
            // Make it look like a parcel: a plausible envelope around noise.
            payload = topics::wrap(&payload).unwrap_or(payload);
        }
        match decode(&topic, &payload) {
            Body::Decoded { .. } => decoded += 1,
            Body::Undecodable { .. } => refused += 1,
            Body::Bytes { bytes, len } => assert!(bytes.len() <= len.max(MAX_KEPT)),
        }
    }
    assert!(refused > 0);
    let _ = decoded;
}

#[test]
fn damaging_a_valid_payload_never_panics() {
    let valid = tick_payload(42);
    let mut rng = Rng(seed(0xbad0_f00d));
    for _ in 0..cases(20_000) {
        let mut bytes = valid.clone();
        match rng.below(4) {
            0 => bytes.truncate(rng.below(bytes.len() as u64 + 1) as usize),
            1 => {
                let at = rng.below(bytes.len() as u64) as usize;
                bytes[at] ^= 1 << rng.below(8);
            }
            2 => {
                let n = rng.below(16) as usize;
                bytes.extend(rng.bytes(n));
            }
            _ => {
                let at = rng.below(bytes.len() as u64) as usize;
                bytes[at] = rng.next() as u8;
            }
        }
        let _ = decode("time/tick", &bytes);
        let _ = decode("system/stats/tasks", &bytes);
    }
}
