//! Byte-level fuzzing of the decoder.
//!
//! [`run`] is the shared entry point for the cargo-fuzz target
//! (`fuzz/fuzz_targets/ipp.rs`) and the seeded tests below. Whatever the
//! bytes, it must not panic, and a message that decodes must re-encode and
//! decode back to itself.

use crate::request::{markers, JobStatus};
use crate::Message;

/// Decode `input`; when it is a message, check the round trip and read it
/// the way the print client does.
pub fn run(input: &[u8]) {
    let Ok((message, end)) = Message::decode(input) else {
        return;
    };
    assert!(end <= input.len());
    let bytes = message.encode().expect("a decoded message re-encodes");
    let (again, again_end) = Message::decode(&bytes).expect("re-encoded bytes decode");
    assert_eq!(again, message);
    assert_eq!(again_end, bytes.len());
    let _ = JobStatus::of(&message);
    let _ = markers(&message);
}

#[cfg(test)]
mod seeded {
    use super::*;
    use crate::{tag, Attribute, Group, Value};
    use alloc::string::String;
    use alloc::vec::Vec;
    use fuzzkit::{for_seeds, Rng};

    fn word(rng: &mut Rng) -> String {
        let len = rng.range(1, 12) as usize;
        (0..len)
            .map(|_| char::from(b'a' + rng.below(26) as u8))
            .collect()
    }

    fn value(rng: &mut Rng, depth: usize) -> Value {
        match rng.below(if depth < 3 { 9 } else { 8 }) {
            0 => Value::Integer(rng.next_u32() as i32),
            1 => Value::Boolean(rng.one_in(2)),
            2 => Value::Enum(rng.range(3, 10) as i32),
            3 => Value::Resolution {
                x: 300,
                y: rng.range(1, 1200) as i32,
                units: 3,
            },
            4 => Value::Range {
                lower: 1,
                upper: rng.range(1, 100) as i32,
            },
            5 => Value::WithLanguage {
                tag: tag::TEXT_WITH_LANGUAGE,
                language: String::from("en"),
                text: word(rng),
            },
            6 => Value::OutOfBand(*rng.pick(&[tag::UNSUPPORTED, tag::UNKNOWN, tag::NO_VALUE])),
            7 => Value::string(
                *rng.pick(&[tag::KEYWORD, tag::URI, tag::NAME, tag::TEXT]),
                &word(rng),
            ),
            _ => Value::Collection(
                (0..rng.range(1, 4))
                    .map(|_| attribute(rng, depth + 1))
                    .collect(),
            ),
        }
    }

    fn attribute(rng: &mut Rng, depth: usize) -> Attribute {
        let values = (0..rng.range(1, 4)).map(|_| value(rng, depth)).collect();
        Attribute::set(&word(rng), values)
    }

    fn message(rng: &mut Rng) -> Message {
        let mut message = Message::new(rng.next_u32() as u16, rng.next_u32());
        for _ in 0..rng.range(1, 4) {
            let mut group = Group::new(*rng.pick(&[tag::OPERATION, tag::JOB, tag::PRINTER]));
            group.attributes = (0..rng.range(0, 6)).map(|_| attribute(rng, 0)).collect();
            message.groups.push(group);
        }
        message
    }

    #[test]
    fn random_messages_round_trip() {
        for_seeds("ipp_random_messages_round_trip", |_, rng| {
            let m = message(rng);
            let bytes = m.encode().unwrap();
            assert_eq!(Message::decode(&bytes).unwrap(), (m, bytes.len()));
            run(&bytes);
        });
    }

    #[test]
    fn mutated_messages_never_panic() {
        for_seeds("ipp_mutated_messages_never_panic", |_, rng| {
            let mut bytes = message(rng).encode().unwrap();
            let flips = rng.range(1, 8) as usize;
            rng.flip_bits(&mut bytes, flips);
            if rng.one_in(3) {
                let cut = rng.below(bytes.len() as u64 + 1) as usize;
                bytes.truncate(cut);
            }
            run(&bytes);
        });
    }

    #[test]
    fn noise_never_panics() {
        for_seeds("ipp_noise_never_panics", |_, rng| {
            let len = rng.below(200) as usize;
            let bytes: Vec<u8> = rng.bytes(len);
            run(&bytes);
        });
    }
}

/// The checked-in cargo-fuzz seeds (`fuzz/gen_corpus.py`) replay here, and
/// every one but `empty` is a message the decoder takes.
#[cfg(test)]
#[test]
fn the_fuzz_seeds_replay() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/ipp");
    let mut seen = 0;
    for entry in std::fs::read_dir(dir).expect("fuzz/seeds/ipp exists") {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        run(&bytes);
        if !path.ends_with("empty") {
            assert!(Message::decode(&bytes).is_ok(), "{}", path.display());
        }
        seen += 1;
    }
    assert!(seen >= 5);
}
