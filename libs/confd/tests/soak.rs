//! Soak: many randomized operations against the store, with periodic
//! encode/decode round trips and a final persist/load round trip.

mod common;

use std::collections::BTreeMap;

use common::{MemoryFs, SplitMix64, ROOT};
use confd::{decode, encode, load, persist, Error, Store, Value, MAX_VALUE_LEN};

const OPS: usize = 100_000;
const CHECKPOINT: usize = 10_000;

fn random_value(rng: &mut SplitMix64) -> Value {
    match rng.below(6) {
        0 => Value::Bool(rng.below(2) == 1),
        1 => Value::I64(rng.next_u64() as i64),
        2 => Value::U64(rng.next_u64()),
        3 => {
            let len = rng.below(32) as usize;
            Value::Str(
                (0..len)
                    .map(|_| char::from(b'a' + rng.below(26) as u8))
                    .collect(),
            )
        }
        4 => {
            let len = rng.below(64) as usize;
            Value::Bytes((0..len).map(|_| rng.below(256) as u8).collect())
        }
        // Occasionally push near the value limit, so the store can approach
        // the total limit and exercise TooLarge.
        _ => {
            let len = MAX_VALUE_LEN - 8 + rng.below(16) as usize;
            Value::Bytes(vec![rng.below(256) as u8; len])
        }
    }
}

#[test]
fn random_ops_stay_consistent_with_a_mirror() {
    let mut rng = SplitMix64::new(0xC0FF_EE00);
    let mut store = Store::new();
    let mut mirror: BTreeMap<String, Value> = BTreeMap::new();
    let mut too_large = 0usize;

    let paths: Vec<String> = (0..300)
        .map(|index| match index % 3 {
            0 => format!("sys/net/{index}/mtu"),
            1 => format!("user/1000/app{index}/theme"),
            _ => format!("user/1001/app{index}/size"),
        })
        .collect();

    for step in 0..OPS {
        let path = &paths[rng.below(paths.len() as u64) as usize];
        match rng.below(10) {
            0..=5 => {
                let value = random_value(&mut rng);
                match store.set(path, value.clone(), ROOT) {
                    Ok(change) => {
                        assert_eq!(change.path, *path);
                        assert_eq!(change.new, Some(value.clone()));
                        mirror.insert(path.clone(), value);
                    }
                    Err(Error::TooLarge) => too_large += 1,
                    Err(other) => panic!("unexpected set error {other:?}"),
                }
            }
            6..=8 => {
                let existed = mirror.remove(path).is_some();
                match store.delete(path, ROOT) {
                    Ok(change) => assert_eq!(change.is_some(), existed),
                    Err(other) => panic!("unexpected delete error {other:?}"),
                }
            }
            _ => assert_eq!(store.get(path, ROOT).unwrap(), mirror.get(path)),
        }

        if step % CHECKPOINT == 0 {
            assert_eq!(decode(&encode(&store)).unwrap(), store, "at step {step}");
        }
    }

    assert_eq!(decode(&encode(&store)).unwrap(), store);
    assert_eq!(store.len(), mirror.len());
    assert_eq!(store.list("", ROOT).unwrap().len(), mirror.len());
    for (path, value) in &mirror {
        assert_eq!(store.get(path, ROOT).unwrap(), Some(value), "{path}");
    }
    assert!(too_large > 0, "the soak never reached the total limit");

    let mut fs = MemoryFs::new();
    persist(&mut fs, &store).unwrap();
    assert_eq!(load(&mut fs).unwrap(), store);
}
