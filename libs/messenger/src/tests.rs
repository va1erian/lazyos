//! Host tests for the parcel codec.

use alloc::vec::Vec;

use super::*;

fn sample_parcel() -> Parcel {
    let mut body = Encoder::new();
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
    body.handle(15, 0xdead_beef).unwrap();
    body.buffer(
        16,
        &BufferDesc {
            handle: 7,
            offset: 4096,
            len: 8192,
            flags: 3,
        },
    )
    .unwrap();

    Parcel {
        header: Header {
            version: VERSION,
            flags: flags::SYNC | flags::TRACE,
            interface_id: 0x1234_5678_9abc_def0,
            method: 9,
            txn_id: 0xfeed,
            reply_to: 0,
            deadline_ns: 1_000_000,
        },
        body: body.finish(),
        handles: vec![1, 2, 3],
        buffers: vec![BufferDesc {
            handle: 7,
            offset: 4096,
            len: 8192,
            flags: 3,
        }],
    }
}

#[test]
fn round_trip_parcel() {
    let parcel = sample_parcel();
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).unwrap();
    let decoded = Parcel::decode(&bytes).unwrap();
    assert_eq!(decoded, parcel);
}

#[test]
fn every_kind_decodes() {
    let parcel = sample_parcel();
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).unwrap();
    let decoded = Parcel::decode(&bytes).unwrap();

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
            Kind::Array => {
                let mut nested = field.nested(0).unwrap();
                assert_eq!(nested.count(), 2);
            }
            Kind::Struct => {
                let mut nested = field.nested(0).unwrap();
                assert_eq!(nested.count(), 1);
            }
            Kind::Map => {
                let mut nested = field.nested(0).unwrap();
                assert_eq!(nested.count(), 2);
            }
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
            Kind::Handle => assert_eq!(field.as_handle().unwrap(), 0xdead_beef),
            Kind::Buffer => {
                let buffer = field.as_buffer().unwrap();
                assert_eq!(buffer.len, 8192);
            }
        }
        seen += 1;
    }
    assert_eq!(seen, 16, "every kind should be exercised");
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
    // Too many handles.
    let mut parcel = sample_parcel();
    parcel.handles = vec![0; MAX_HANDLES + 1];
    let mut bytes = Vec::new();
    assert_eq!(parcel.encode(&mut bytes), Err(Error::TooLarge));

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
    let mut bytes2 = Vec::new();
    parcel.encode(&mut bytes2).unwrap();
    bytes2[0] = 99;
    assert_eq!(Parcel::decode(&bytes2), Err(Error::BadVersion));
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
    let valid = {
        let mut bytes = Vec::new();
        sample_parcel().encode(&mut bytes).unwrap();
        bytes
    };

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

        // The contract: no panic, ever. Contents may be Ok or Err.
        let _ = Parcel::decode(&bytes);
        if let Ok(parcel) = Parcel::decode(&bytes) {
            let mut decoder = Decoder::new(&parcel.body);
            while let Ok(Some(field)) = decoder.next() {
                let _ = field.as_u64();
                let _ = field.as_str();
                let _ = field.nested(0);
                let _ = field.error_parts();
            }
        }
    }
}
