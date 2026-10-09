//! Host tests for the parcel codec.

use alloc::vec::Vec;

use super::*;

/// A parcel of every field kind: two object fields, `events` (a channel at
/// index 0) and `pixels` (a buffer at index 1), in that declared order.
fn sample_parcel() -> Parcel {
    let mut body = Encoder::new();
    let mut objects = Vec::new();
    body.bool(1, true).unwrap();
    body.i32(2, -7).unwrap();
    body.i64(3, -9_000_000_000).unwrap();
    body.u32(4, 42).unwrap();
    body.u64(5, u64::MAX).unwrap();
    body.f64(6, 3.5).unwrap();
    body.string(7, "hello messenger").unwrap();
    body.bytes(8, &[0, 1, 2, 255]).unwrap();
    let mut inner = Encoder::new();
    inner.u32(1, 10).unwrap();
    inner.u32(2, 20).unwrap();
    body.array(9, &inner).unwrap();
    let mut record = Encoder::new();
    record.string(1, "name").unwrap();
    body.record(10, &record).unwrap();
    let mut map = Encoder::new();
    map.u32(1, 1).unwrap();
    map.u32(2, 2).unwrap();
    body.map(11, &map).unwrap();
    body.option(12, None).unwrap();
    body.option(13, Some(&record)).unwrap();
    body.error(14, 38, "not implemented").unwrap();
    body.channel(15, 0xdead_beef, &mut objects).unwrap();
    body.buffer(
        16,
        &Buffer {
            handle: 7,
            offset: 4096,
            len: 8192,
        },
        &mut objects,
    )
    .unwrap();

    Parcel {
        header: Header {
            version: VERSION,
            flags: flags::SYNC | flags::ALLOW_NESTED,
            interface_id: 0x1234_5678_9abc_def0,
            method: 9,
            txn_id: 0xfeed,
            reply_to: 0,
            deadline_ns: 1_000_000,
        },
        body: body.finish(),
        objects,
    }
}

/// The receiver's view of the sample's objects: installed numbers.
fn installed() -> Vec<Object> {
    vec![Object::Channel(3), Object::Buffer(4)]
}

fn encoded(parcel: &Parcel) -> Vec<u8> {
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).unwrap();
    bytes
}

#[test]
fn round_trip_parcel() {
    let parcel = sample_parcel();
    assert_eq!(
        parcel.objects,
        vec![Object::Channel(0xdead_beef), Object::Buffer(7)]
    );
    let decoded = Parcel::decode(&encoded(&parcel)).unwrap();
    assert_eq!(decoded, parcel);
}

#[test]
fn header_is_forty_eight_bytes_in_the_plan_order() {
    let bytes = encoded(&sample_parcel());
    assert_eq!(&bytes[0..2], &2u16.to_le_bytes());
    assert_eq!(&bytes[2..4], &(flags::SYNC | flags::ALLOW_NESTED).to_le_bytes());
    assert_eq!(&bytes[4..8], &2u32.to_le_bytes(), "object_count");
    assert_eq!(&bytes[8..16], &0x1234_5678_9abc_def0u64.to_le_bytes());
    assert_eq!(&bytes[16..20], &9u32.to_le_bytes(), "method");
    assert_eq!(&bytes[24..32], &0xfeedu64.to_le_bytes(), "txn_id");
    assert_eq!(&bytes[40..48], &1_000_000u64.to_le_bytes(), "deadline");
    // The object list follows the body: kind, reserved, handle.
    let list = bytes.len() - 2 * OBJECT_ENTRY_SIZE;
    assert_eq!(&bytes[list..list + 4], &1u32.to_le_bytes());
    assert_eq!(&bytes[list + 4..list + 8], &0u32.to_le_bytes());
    assert_eq!(&bytes[list + 8..list + 16], &0xdead_beefu64.to_le_bytes());
    assert_eq!(&bytes[list + 16..list + 20], &2u32.to_le_bytes());
}

#[test]
fn view_reads_in_place() {
    let parcel = sample_parcel();
    let bytes = encoded(&parcel);
    let view = ParcelView::parse(&bytes).unwrap();
    assert_eq!(view.header, parcel.header);
    assert_eq!(view.body(), &parcel.body[..]);
    assert_eq!(view.object_count(), 2);
    assert_eq!(view.objects().collect::<Vec<_>>(), parcel.objects);
    // The body slice is the caller's bytes, not a copy.
    assert!(core::ptr::eq(
        view.body().as_ptr(),
        bytes[HEADER_SIZE..].as_ptr()
    ));
    assert_eq!(
        ParcelView::parse(&bytes[..bytes.len() - 1]),
        Err(Error::Truncated)
    );
}

#[test]
fn every_kind_decodes() {
    let decoded = Parcel::decode(&encoded(&sample_parcel())).unwrap();
    let objects = installed();
    let mut next = 0;
    let mut seen = 0usize;
    let mut decoder = Decoder::new(&decoded.body);
    while let Some(field) = decoder.next().unwrap() {
        match field.kind {
            Kind::Bool => assert!(field.as_bool().unwrap()),
            Kind::I32 => assert_eq!(field.as_i32().unwrap(), -7),
            Kind::I64 => assert_eq!(field.as_i64().unwrap(), -9_000_000_000),
            Kind::U32 => assert_eq!(field.as_u32().unwrap(), 42),
            Kind::U64 => assert_eq!(field.as_u64().unwrap(), u64::MAX),
            Kind::F64 => assert_eq!(field.as_f64().unwrap(), 3.5),
            Kind::String => assert_eq!(field.as_str().unwrap(), "hello messenger"),
            Kind::Bytes => assert_eq!(field.as_bytes(), &[0, 1, 2, 255]),
            Kind::Array => assert_eq!(field.nested(0).unwrap().count(), 2),
            Kind::Struct => assert_eq!(field.nested(0).unwrap().count(), 1),
            Kind::Map => assert_eq!(field.nested(0).unwrap().count(), 2),
            Kind::Option => {
                let mut nested = field.nested(0).unwrap();
                assert_eq!(
                    nested.count() as u8,
                    if field.payload.is_empty() { 0 } else { 1 }
                );
            }
            Kind::Error => {
                let (code, message) = field.error_parts().unwrap();
                assert_eq!((code, message), (38, "not implemented"));
            }
            Kind::Handle => {
                assert_eq!(field.object_index().unwrap(), 0);
                // The receiver sees the installed number, not the sender's.
                assert_eq!(field.claim_channel(&objects, &mut next).unwrap(), 3);
            }
            Kind::Buffer => {
                assert_eq!(field.buffer_parts().unwrap(), (1, 4096, 8192));
                let buffer = field.claim_buffer(&objects, &mut next).unwrap();
                assert_eq!(
                    buffer,
                    Buffer {
                        handle: 4,
                        offset: 4096,
                        len: 8192
                    }
                );
            }
        }
        seen += 1;
    }
    assert_eq!(seen, 16, "every kind should be exercised");
    assert_eq!(next, objects.len(), "every object was claimed");
}

/// The index rule: a field's index must be its position in the declared
/// order, so a repeated, skipped, out-of-range or wrong-kind index is refused
/// and `next` does not move.
#[test]
fn object_index_rule() {
    let objects = installed();
    let field = |kind: Kind, payload: &'static [u8]| Field {
        kind,
        id: 1,
        payload,
    };
    let channel = |index: u32| -> &'static [u8] { Vec::leak(index.to_le_bytes().to_vec()) };
    let buffer = |index: u32| -> &'static [u8] {
        let mut payload = index.to_le_bytes().to_vec();
        payload.extend_from_slice(&0u64.to_le_bytes());
        payload.extend_from_slice(&16u64.to_le_bytes());
        Vec::leak(payload)
    };

    // In order: 0 then 1.
    let mut next = 0;
    assert_eq!(
        field(Kind::Handle, channel(0)).claim_channel(&objects, &mut next),
        Ok(3)
    );
    assert_eq!(
        field(Kind::Buffer, buffer(1)).claim_buffer(&objects, &mut next),
        Ok(Buffer::whole(4, 16))
    );
    assert_eq!(next, 2);
    // Out of range: a third claim.
    assert_eq!(
        field(Kind::Handle, channel(2)).claim_channel(&objects, &mut next),
        Err(Error::BadObjectIndex)
    );
    // Repeated: index 0 again.
    assert_eq!(
        field(Kind::Handle, channel(0)).claim_channel(&objects, &mut next),
        Err(Error::BadObjectIndex)
    );
    assert_eq!(next, 2, "a refused claim does not move");
    // Out of order: the buffer (slot 1) claimed first.
    let mut next = 0;
    assert_eq!(
        field(Kind::Buffer, buffer(1)).claim_buffer(&objects, &mut next),
        Err(Error::BadObjectIndex)
    );
    // Wrong kind: a buffer field naming the channel slot.
    assert_eq!(
        field(Kind::Buffer, buffer(0)).claim_buffer(&objects, &mut next),
        Err(Error::BadObjectIndex)
    );
    assert_eq!(
        field(Kind::Handle, channel(0)).claim_channel(&[Object::Buffer(9)], &mut next),
        Err(Error::BadObjectIndex)
    );
    // Past an empty list.
    assert_eq!(
        field(Kind::Handle, channel(0)).claim_channel(&[], &mut next),
        Err(Error::BadObjectIndex)
    );
    assert_eq!(next, 0);
    // A malformed payload is a value error, not an index error.
    assert_eq!(
        field(Kind::Handle, &[1, 2, 3]).claim_channel(&objects, &mut next),
        Err(Error::BadValue)
    );
    assert_eq!(
        field(Kind::Buffer, &[0; 8]).claim_buffer(&objects, &mut next),
        Err(Error::BadValue)
    );
}

#[test]
fn a_buffer_range_is_checked_against_the_mapped_size() {
    assert!(Buffer::whole(1, 4096).fits(4096));
    assert!(!Buffer::whole(1, 4097).fits(4096));
    assert!(Buffer {
        handle: 1,
        offset: 4096,
        len: 0
    }
    .fits(4096));
    assert!(!Buffer {
        handle: 1,
        offset: 1,
        len: 4096
    }
    .fits(4096));
    assert!(!Buffer {
        handle: 1,
        offset: u64::MAX,
        len: 1
    }
    .fits(u64::MAX));
}

#[test]
fn unknown_kinds_are_skipped() {
    // A field with kind 200 (unknown) followed by a known `u32`.
    let mut body = Vec::new();
    body.extend_from_slice(&(200u32 | (1 << 8)).to_le_bytes());
    body.extend_from_slice(&4u32.to_le_bytes());
    body.extend_from_slice(&[9, 9, 9, 9]);
    let mut encoder = Encoder::new();
    encoder.u32(7, 1234).unwrap();
    body.extend_from_slice(encoder.as_bytes());

    let mut decoder = Decoder::new(&body);
    let field = decoder.next().unwrap().unwrap();
    assert_eq!(field.kind, Kind::U32);
    assert_eq!(field.id, 7);
    assert_eq!(field.as_u32().unwrap(), 1234);
    assert!(decoder.next().unwrap().is_none());
}

#[test]
fn limits_are_enforced() {
    // Too many objects, in the parcel and in the encoder.
    let mut parcel = sample_parcel();
    parcel.objects = vec![Object::Channel(0); MAX_OBJECTS + 1];
    let mut bytes = Vec::new();
    assert_eq!(parcel.encode(&mut bytes), Err(Error::TooLarge));
    let mut objects = vec![Object::Buffer(1); MAX_OBJECTS];
    assert_eq!(
        Encoder::new().channel(1, 5, &mut objects),
        Err(Error::TooLarge)
    );

    // Body over the limit.
    let mut parcel = sample_parcel();
    parcel.body = vec![0; MAX_BODY_BYTES + 1];
    assert_eq!(parcel.encode(&mut bytes), Err(Error::TooLarge));

    // Trailing bytes after a valid parcel.
    let parcel = sample_parcel();
    parcel.encode(&mut bytes).unwrap();
    bytes.push(0);
    assert_eq!(Parcel::decode(&bytes), Err(Error::TrailingBytes));

    // Bad version.
    let mut bytes = encoded(&parcel);
    bytes[0] = 1;
    assert_eq!(Parcel::decode(&bytes), Err(Error::BadVersion));

    // An unknown object kind, and a reserved word that is not zero.
    let list = encoded(&parcel).len() - 2 * OBJECT_ENTRY_SIZE;
    let mut bytes = encoded(&parcel);
    bytes[list] = 3;
    assert_eq!(Parcel::decode(&bytes), Err(Error::BadObject));
    let mut bytes = encoded(&parcel);
    bytes[list + OBJECT_ENTRY_SIZE + 4] = 1;
    assert_eq!(Parcel::decode(&bytes), Err(Error::BadObject));
    // An object count past the list.
    let mut bytes = encoded(&parcel);
    bytes[4] = 3;
    assert_eq!(Parcel::decode(&bytes), Err(Error::Truncated));
}

/// A tiny deterministic PRNG (xorshift64*), so the fuzz test needs no deps.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

#[test]
fn fuzz_decode_never_panics() {
    const ITERATIONS: usize = 1_000_000;
    let mut rng = Rng(0x1234_5678_9abc_def0);
    let valid = encoded(&sample_parcel());

    for i in 0..ITERATIONS {
        // Start from random bytes or a mutated valid parcel.
        let mut bytes = if i % 2 == 0 {
            let len = (rng.next() % 256) as usize;
            let mut v = vec![0u8; len];
            for b in v.iter_mut() {
                *b = rng.next() as u8;
            }
            v
        } else {
            valid.clone()
        };
        let mutations = rng.next() % 8;
        for _ in 0..mutations {
            if bytes.is_empty() {
                break;
            }
            let at = (rng.next() as usize) % bytes.len();
            bytes[at] = rng.next() as u8;
        }
        // Truncate sometimes.
        if rng.next().is_multiple_of(4) && !bytes.is_empty() {
            let keep = (rng.next() as usize) % bytes.len();
            bytes.truncate(keep);
        }
        // The contract: no panic, ever, and the view agrees with decode.
        crate::fuzz::run(&bytes);
    }
}

#[test]
fn an_error_detail_record_rides_after_the_message() {
    let mut detail = Encoder::new();
    detail.string(1, "os.lazy.fs").unwrap();
    let mut body = Encoder::new();
    body.error_detail(15, 13, "denied", &detail).unwrap();
    let bytes = body.finish();
    let field = Decoder::new(&bytes).next().unwrap().unwrap();
    assert_eq!(field.error_parts().unwrap(), (13, "denied"));
    let mut record = field.error_detail().unwrap();
    let domain = record.next().unwrap().unwrap();
    assert_eq!((domain.id, domain.as_str().unwrap()), (1, "os.lazy.fs"));
    assert!(record.next().unwrap().is_none());
}

#[test]
fn a_plain_error_has_no_detail_and_a_nul_message_is_refused() {
    let mut body = Encoder::new();
    body.error(15, 2, "gone").unwrap();
    let bytes = body.finish();
    let field = Decoder::new(&bytes).next().unwrap().unwrap();
    assert!(field.error_detail().is_none());
    let mut refused = Encoder::new();
    let detail = Encoder::new();
    assert_eq!(
        refused.error_detail(15, 2, "a\0b", &detail),
        Err(Error::BadString)
    );
}
