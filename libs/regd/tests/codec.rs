//! Encoding format: round trips and rejection of every malformed shape.

mod common;

use common::{blob, envelope, text, SplitMix64, ROOT};
use regd::{
    decode, encode, DecodeError, Store, Value, MAX_ENCODED_LEN, MAX_PATH_LEN, MAX_VALUE_LEN,
};

fn sample_store() -> Store {
    let mut store = Store::new();
    store.set("sys/flag", Value::Bool(true), ROOT).unwrap();
    store.set("sys/off", Value::Bool(false), ROOT).unwrap();
    store.set("sys/min", Value::I64(i64::MIN), ROOT).unwrap();
    store.set("sys/max", Value::U64(u64::MAX), ROOT).unwrap();
    store.set("sys/empty-str", text(""), ROOT).unwrap();
    store
        .set("sys/utf8", text("h\u{e9}llo \u{2192} \u{1f600}"), ROOT)
        .unwrap();
    store
        .set("user/1000/blob", blob(&[0, 255, 1, 254]), ROOT)
        .unwrap();
    store
        .set("user/1000/empty-blob", Value::Bytes(Vec::new()), ROOT)
        .unwrap();
    store
}

/// One entry as the format defines it, without any validation checks.
fn raw_entry(path: &[u8], tag: u8, value: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(path.len() as u16).to_le_bytes());
    body.extend_from_slice(path);
    body.push(tag);
    body.extend_from_slice(value);
    body
}

#[test]
fn round_trip_all_value_types() {
    let store = sample_store();
    let encoded = encode(&store);
    assert_eq!(decode(&encoded), Ok(store.clone()));
    // Encoding is deterministic.
    assert_eq!(encode(&store), encoded);
}

#[test]
fn empty_store_round_trips() {
    let store = Store::new();
    let encoded = encode(&store);
    assert_eq!(encoded.len(), 8);
    assert_eq!(decode(&encoded), Ok(store));
}

#[test]
fn truncation_is_rejected_at_every_length() {
    let full = encode(&sample_store());
    assert_eq!(decode(&full[..4]), Err(DecodeError::TooShort));
    for len in 0..full.len() {
        assert!(
            decode(&full[..len]).is_err(),
            "truncated input of {len} bytes decoded"
        );
    }
}

#[test]
fn single_byte_corruption_is_rejected() {
    let full = encode(&sample_store());
    for index in 0..full.len() {
        let mut corrupt = full.clone();
        corrupt[index] ^= 0xFF;
        assert!(
            decode(&corrupt).is_err(),
            "corruption at byte {index} decoded"
        );
    }
}

#[test]
fn bad_magic_and_crc_are_distinguished() {
    let mut corrupt = encode(&sample_store());
    corrupt[0] = b'X';
    assert_eq!(decode(&corrupt), Err(DecodeError::BadMagic));

    let mut corrupt = encode(&sample_store());
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0xFF;
    assert_eq!(decode(&corrupt), Err(DecodeError::BadCrc));
}

#[test]
fn oversized_input_is_rejected_before_parsing() {
    let huge = vec![0u8; MAX_ENCODED_LEN + 1];
    assert_eq!(decode(&huge), Err(DecodeError::TooLarge));
}

#[test]
fn empty_body_with_valid_crc_is_an_empty_store() {
    assert_eq!(decode(&envelope(&[])), Ok(Store::new()));
}

#[test]
fn structural_errors_are_rejected_even_with_valid_crc() {
    let cases: [(Vec<u8>, DecodeError); 10] = [
        // Entry cut off after the path length.
        (vec![0x01, 0x00], DecodeError::Malformed),
        // A zero-length path.
        (raw_entry(b"", 0, &[1]), DecodeError::BadPath),
        // Path with an empty segment.
        (raw_entry(b"sys/", 0, &[1]), DecodeError::BadPath),
        // Path that is not sys/user.
        (raw_entry(b"etc/passwd", 0, &[1]), DecodeError::BadPath),
        // Unknown value tag.
        (raw_entry(b"sys/a", 9, &[]), DecodeError::Malformed),
        // Bool must be exactly 0 or 1.
        (raw_entry(b"sys/a", 0, &[2]), DecodeError::Malformed),
        // Fixed-width i64 cut short.
        (
            raw_entry(b"sys/a", 1, &[0, 0, 0, 0]),
            DecodeError::Malformed,
        ),
        // Blob length running past the end.
        (
            raw_entry(b"sys/a", 4, &[8, 0, 0, 0, 1]),
            DecodeError::Malformed,
        ),
        // Blob over the value limit (declared, so checked before reading).
        (
            raw_entry(b"sys/a", 3, &[0x01, 0x10, 0x00, 0x00]),
            DecodeError::TooLarge,
        ),
        // Non-UTF-8 string.
        (
            raw_entry(b"sys/a", 3, &[2, 0, 0, 0, 0xFF, 0xFE]),
            DecodeError::Malformed,
        ),
    ];
    for (body, expected) in cases {
        assert_eq!(decode(&envelope(&body)), Err(expected), "{expected:?}");
    }
}

#[test]
fn duplicate_paths_are_rejected() {
    let mut body = raw_entry(b"sys/a", 0, &[1]);
    body.extend_from_slice(&raw_entry(b"sys/a", 2, &[0; 8]));
    assert_eq!(decode(&envelope(&body)), Err(DecodeError::Duplicate));
}

#[test]
fn trailing_junk_after_entries_is_rejected() {
    let mut body = raw_entry(b"sys/a", 0, &[1]);
    body.push(0x00);
    assert!(decode(&envelope(&body)).is_err());
}

#[test]
fn path_length_limit_is_rechecked_from_the_wire() {
    let mut body = Vec::new();
    body.extend_from_slice(&((MAX_PATH_LEN + 1) as u16).to_le_bytes());
    body.extend_from_slice(&vec![b'a'; MAX_PATH_LEN + 1]);
    body.push(0);
    body.push(1);
    assert_eq!(decode(&envelope(&body)), Err(DecodeError::BadPath));
}

#[test]
fn total_size_limit_is_rechecked_from_the_wire() {
    // Distinct paths with max-size blobs summing past 1 MiB.
    let mut body = Vec::new();
    for index in 0..260 {
        let path = format!("sys/p{index:04}");
        let mut value = (MAX_VALUE_LEN as u32).to_le_bytes().to_vec();
        value.resize(4 + MAX_VALUE_LEN, 0);
        body.extend_from_slice(&raw_entry(path.as_bytes(), 4, &value));
    }
    assert_eq!(decode(&envelope(&body)), Err(DecodeError::TooLarge));
}

#[test]
fn max_length_path_is_accepted_from_the_wire() {
    let path = format!("sys/{}", "a".repeat(MAX_PATH_LEN - 4));
    assert_eq!(path.len(), MAX_PATH_LEN);
    let store = decode(&envelope(&raw_entry(path.as_bytes(), 0, &[1]))).unwrap();
    assert_eq!(store.get(&path, ROOT).unwrap(), Some(&Value::Bool(true)));
}

#[test]
fn mutated_body_with_valid_crc_never_panics_and_reencodes_stably() {
    // Strip the magic and CRC and re-wrap each mutation with a fresh
    // checksum, so the parser — not the CRC — is what has to reject it; the
    // header/trailer bytes themselves are covered by the corruption test.
    let encoded = encode(&sample_store());
    let body = &encoded[4..encoded.len() - 4];
    let mut rng = SplitMix64::new(0x5EED);
    for _ in 0..20_000 {
        let mut mutated = body.to_vec();
        let flips = 1 + rng.below(4) as usize;
        for _ in 0..flips {
            let index = rng.below(mutated.len() as u64) as usize;
            mutated[index] ^= (1 << rng.below(8)) as u8;
        }
        if let Ok(store) = decode(&envelope(&mutated)) {
            let reencoded = encode(&store);
            assert_eq!(decode(&reencoded).unwrap(), store);
        }
    }
}

#[test]
fn random_bytes_with_valid_crc_never_panic() {
    let mut rng = SplitMix64::new(0x1234_5678);
    for _ in 0..10_000 {
        let mut body = vec![0u8; rng.below(96) as usize];
        for byte in &mut body {
            *byte = rng.below(256) as u8;
        }
        let _ = decode(&envelope(&body));
    }
}
