//! The parcel decoder against arbitrary bytes.
//!
//! [`run`] is the one entry point the libFuzzer target
//! (`fuzz/fuzz_targets/messenger.rs`) and the seeded tests below share, so a
//! crash found by one replays under the other. The contract is total
//! robustness: decoding never panics, the in-place view and the owned decode
//! agree byte for byte, every field accessor tolerates every payload, and
//! the object index rule holds whatever the body says.

use std::vec::Vec;

use crate::{Decoder, Error, Field, Kind, Object, Parcel, ParcelView};

/// Decode `data` every way the library offers; panic on any broken invariant.
pub fn run(data: &[u8]) {
    let decoded = Parcel::decode(data);
    match (ParcelView::parse(data), &decoded) {
        (Ok(view), Ok(parcel)) => {
            assert_eq!(view.header, parcel.header);
            assert_eq!(view.body(), &parcel.body[..]);
            assert_eq!(view.object_count(), parcel.objects.len());
            assert!(view.objects().eq(parcel.objects.iter().copied()));
        }
        (Err(left), Err(right)) => assert_eq!(left, *right),
        (view, parcel) => panic!("view {view:?} disagrees with decode {parcel:?}"),
    }
    let Ok(parcel) = decoded else {
        return;
    };
    // The receiver's list: the same kinds with installed numbers.
    let installed: Vec<Object> = parcel
        .objects
        .iter()
        .enumerate()
        .map(|(index, object)| object.with_handle(1000 + index as u64))
        .collect();
    let mut next = 0usize;
    walk(&parcel.body, &installed, &mut next, 0);
    // Whatever the fields claimed, they claimed in order and never past the list.
    assert!(next <= installed.len());
    // Re-encoding what decoded gives the same bytes: the codec is canonical.
    let mut again = Vec::new();
    parcel.encode(&mut again).expect("a decoded parcel encodes");
    assert_eq!(again, data);
}

/// Exercise every accessor of every field, recursing into composites.
fn walk(body: &[u8], objects: &[Object], next: &mut usize, depth: u8) {
    let mut decoder = Decoder::new(body);
    while let Ok(Some(field)) = decoder.next() {
        touch(&field, objects, next);
        if matches!(
            field.kind,
            Kind::Array | Kind::Struct | Kind::Map | Kind::Option
        ) {
            if let Ok(nested) = field.nested(depth) {
                let _ = nested;
                walk(field.payload, objects, next, depth + 1);
            }
        }
        if let Some(mut detail) = field.error_detail() {
            while let Ok(Some(_)) = detail.next() {}
        }
    }
}

fn touch(field: &Field<'_>, objects: &[Object], next: &mut usize) {
    let _ = field.as_bool();
    let _ = field.as_u32();
    let _ = field.as_u64();
    let _ = field.as_str();
    let _ = field.error_parts();
    let _ = field.object_index();
    let _ = field.buffer_parts();
    let before = *next;
    match field.kind {
        Kind::Handle => check_claim(field.claim_channel(objects, next).map(drop), before, next),
        Kind::Buffer => check_claim(field.claim_buffer(objects, next).map(drop), before, next),
        _ => {}
    }
}

/// A claim advances `next` by exactly one on success and not at all on
/// failure, and a success never names a slot past the list.
fn check_claim(outcome: Result<(), Error>, before: usize, next: &mut usize) {
    match outcome {
        Ok(()) => assert_eq!(*next, before + 1),
        Err(_) => assert_eq!(*next, before),
    }
}

#[cfg(test)]
mod seeded {
    use super::*;
    use crate::{flags, Buffer, Encoder, Header, VERSION};
    use fuzzkit::{for_seeds, Rng};

    /// Replay every checked-in seed and saved crash through the entry point.
    fn replay(target: &str, run: fn(&[u8])) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut seen = 0;
        for dir in ["seeds", "regressions"] {
            let Ok(entries) = std::fs::read_dir(root.join(dir).join(target)) else {
                continue;
            };
            for entry in entries.flatten() {
                run(&std::fs::read(entry.path()).unwrap());
                seen += 1;
            }
        }
        if std::env::var_os("CI").is_some() {
            assert!(seen > 0, "no seeds found for {target}");
        }
    }

    /// A well-formed parcel with a few value fields and `objects` object
    /// fields in declared order, so mutations start near the valid paths.
    fn valid(rng: &mut Rng) -> Vec<u8> {
        let mut body = Encoder::new();
        let mut objects = Vec::new();
        body.u32(1, rng.next_u32()).unwrap();
        body.string(2, "seed").unwrap();
        for id in 0..rng.below(4) as u16 {
            if rng.one_in(2) {
                body.channel(10 + id, rng.next_u64(), &mut objects).unwrap();
            } else {
                let buffer = Buffer::whole(rng.next_u64(), rng.below(1 << 20));
                body.buffer(10 + id, &buffer, &mut objects).unwrap();
            }
        }
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: flags::SYNC,
                interface_id: rng.next_u64(),
                method: rng.next_u32(),
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body: body.finish(),
            objects,
        };
        let mut bytes = Vec::new();
        parcel.encode(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn checked_in_seeds_replay() {
        replay("messenger", run);
    }

    #[test]
    fn mutated_parcels_never_break_the_decoder() {
        for_seeds("messenger", |_seed, rng| {
            let mut bytes = if rng.one_in(3) {
                let len = rng.below(200) as usize;
                rng.bytes(len)
            } else {
                valid(rng)
            };
            let flips = rng.below(6) as usize;
            rng.flip_bits(&mut bytes, flips);
            if rng.one_in(4) && !bytes.is_empty() {
                let keep = rng.below(bytes.len() as u64) as usize;
                bytes.truncate(keep);
            }
            run(&bytes);
        });
    }
}
