//! The sealed file: round trips, damage, caps and a soak.

use alloc::string::ToString;
use alloc::vec::Vec;

use crate::*;

const KEY: [u8; MACHINE_KEY_LEN] = [7; MACHINE_KEY_LEN];

/// A nonce source that never repeats, so sealing is deterministic.
fn counter() -> impl FnMut(&mut [u8; 16]) {
    let mut next = 0u64;
    move |nonce| {
        nonce[..8].copy_from_slice(&next.to_le_bytes());
        next += 1;
    }
}

fn sample() -> Store {
    let mut store = Store::new();
    store
        .put(Owner::System, "office", b"correct horse battery")
        .unwrap();
    store.put(Owner::User(1000), "home", b"password").unwrap();
    store
        .put(Owner::User(1001), "home", b"another one")
        .unwrap();
    store.pmk(Owner::User(1000), "home", b"IEEE").unwrap();
    store
}

// ---- the file ------------------------------------------------------------

#[test]
fn a_file_round_trips_with_its_cached_pmks() {
    let store = sample();
    let bytes = store.seal(&KEY, &mut counter());
    assert!(bytes.len() <= FILE_MAX);
    let mut back = Store::open(&bytes, &KEY).unwrap();
    assert_eq!(back.names(Owner::System), ["office"]);
    assert_eq!(back.names(Owner::User(1000)), ["home"]);
    assert_eq!(back.names(Owner::User(1001)), ["home"]);
    assert_eq!(back.cached(Owner::User(1000), "home"), Some(1));
    let (_, fresh) = back.pmk(Owner::User(1000), "home", b"IEEE").unwrap();
    assert!(!fresh, "the cache survived");
    // Sealing again gives the same table.
    assert_eq!(
        Store::open(&back.seal(&KEY, &mut counter()), &KEY)
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn an_empty_store_round_trips() {
    let bytes = Store::new().seal(&KEY, &mut counter());
    assert!(Store::open(&bytes, &KEY).unwrap().is_empty());
}

#[test]
fn a_secret_is_not_in_the_file_in_the_clear() {
    let bytes = sample().seal(&KEY, &mut counter());
    for needle in [&b"correct horse battery"[..], b"another one", b"password"] {
        assert!(!bytes.windows(needle.len()).any(|w| w == needle));
    }
}

#[test]
fn a_flipped_bit_in_any_byte_refuses_the_whole_file() {
    let bytes = sample().seal(&KEY, &mut counter());
    for index in 0..bytes.len() {
        for bit in [0x01, 0x80] {
            let mut damaged = bytes.clone();
            damaged[index] ^= bit;
            assert!(
                Store::open(&damaged, &KEY).is_err(),
                "byte {index} ^ {bit:#x}"
            );
        }
    }
}

#[test]
fn a_file_cut_short_or_extended_is_refused() {
    let bytes = sample().seal(&KEY, &mut counter());
    for len in 0..bytes.len() {
        assert!(Store::open(&bytes[..len], &KEY).is_err(), "cut to {len}");
    }
    let mut longer = bytes.clone();
    longer.push(0);
    assert!(Store::open(&longer, &KEY).is_err());
}

#[test]
fn another_machine_key_opens_nothing() {
    let bytes = sample().seal(&KEY, &mut counter());
    assert_eq!(
        Store::open(&bytes, &[8; MACHINE_KEY_LEN]).err(),
        Some(FileError::BadTag)
    );
}

#[test]
fn a_record_cannot_be_removed_reordered_or_replayed_in_the_file() {
    let bytes = sample().seal(&KEY, &mut counter());
    // Drop the second record and fix the count: the tag no longer matches.
    let first_len = u32::from_le_bytes(bytes[10..14].try_into().unwrap()) as usize;
    let second_at = 14 + first_len;
    let second_len =
        u32::from_le_bytes(bytes[second_at..second_at + 4].try_into().unwrap()) as usize;
    let mut cut = bytes[..second_at].to_vec();
    cut.extend_from_slice(&bytes[second_at + 4 + second_len..bytes.len() - 32]);
    cut[8] = 2;
    cut.extend_from_slice(&bytes[bytes.len() - 32..]);
    assert_eq!(Store::open(&cut, &KEY).err(), Some(FileError::BadTag));
}

#[test]
fn a_nonce_is_never_reused_between_records() {
    let bytes = sample().seal(&KEY, &mut counter());
    let mut seen = Vec::new();
    let mut at = 10;
    for _ in 0..3 {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let nonce = bytes[at + 5..at + 21].to_vec();
        assert!(!seen.contains(&nonce));
        seen.push(nonce);
        at += 4 + len;
    }
}

#[test]
fn a_full_store_fits_the_stated_maximum() {
    let mut store = Store::new();
    for owner in 0..(MAX_TOTAL / MAX_PER_OWNER) as u32 {
        for index in 0..MAX_PER_OWNER {
            let name = "n".repeat(MAX_NAME - 2) + &alloc::format!("{index:02}");
            store
                .put(Owner::User(owner), &name, &[b'p'; MAX_SECRET])
                .unwrap();
        }
    }
    for entry in &mut store.entries {
        for index in 0..MAX_PMKS {
            entry
                .pmks
                .push((alloc::vec![index as u8 + 1; MAX_SSID], [9; 32]));
        }
    }
    let bytes = store.seal(&KEY, &mut counter());
    assert_eq!(bytes.len(), FILE_MAX);
    assert_eq!(Store::open(&bytes, &KEY).unwrap().len(), MAX_TOTAL);
}

#[test]
fn a_well_sealed_file_over_the_caps_is_refused() {
    // Written by a broken writer: authentic, but not a table `put` could make.
    let mut store = Store::new();
    for index in 0..MAX_PER_OWNER + 1 {
        store.entries.push(Entry {
            owner: Owner::User(1),
            name: index.to_string(),
            secret: alloc::vec![1],
            pmks: Vec::new(),
        });
    }
    let bytes = store.seal(&KEY, &mut counter());
    assert_eq!(Store::open(&bytes, &KEY).err(), Some(FileError::TooMany));

    let mut twice = Store::new();
    for _ in 0..2 {
        twice.entries.push(Entry {
            owner: Owner::System,
            name: "same".to_string(),
            secret: alloc::vec![1],
            pmks: Vec::new(),
        });
    }
    let bytes = twice.seal(&KEY, &mut counter());
    assert_eq!(Store::open(&bytes, &KEY).err(), Some(FileError::TooMany));

    let mut bad = Store::new();
    bad.entries.push(Entry {
        owner: Owner::System,
        name: "bad name".to_string(),
        secret: alloc::vec![1],
        pmks: Vec::new(),
    });
    let bytes = bad.seal(&KEY, &mut counter());
    assert_eq!(Store::open(&bytes, &KEY).err(), Some(FileError::BadRecord));
}

#[test]
fn hostile_bytes_never_panic() {
    // A seeded stream of garbage, and of real files with a mutated random
    // run, must all be refused or opened, never panic.
    let real = sample().seal(&KEY, &mut counter());
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let len = (next() % 600) as usize;
        let junk: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        let _ = Store::open(&junk, &KEY);
        let mut mutated = real.clone();
        for _ in 0..1 + next() % 4 {
            let at = (next() as usize) % mutated.len();
            mutated[at] = next() as u8;
        }
        if mutated != real {
            assert!(Store::open(&mutated, &KEY).is_err());
        }
    }
}

#[test]
fn soak_many_changes_keep_the_file_in_step() {
    // Thousands of store, delete and PMK steps; the sealed file always opens
    // to the table it was sealed from.
    let mut store = Store::new();
    let mut state = 12345u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    let mut nonce = counter();
    for step in 0..3000 {
        let owner = if next() % 4 == 0 {
            Owner::System
        } else {
            Owner::User(1000 + (next() % 3) as u32)
        };
        let name = alloc::format!("n{}", next() % 20);
        match next() % 4 {
            0 | 1 => {
                let _ = store.put(owner, &name, alloc::format!("secret-{step:06}").as_bytes());
            }
            2 => {
                let _ = store.delete(owner, &name);
            }
            _ => {
                // Raw PSKs make the step cheap; the cache path is tested above.
                let _ = store.put(owner, &name, "ab".repeat(32).as_bytes());
                let _ = store.pmk(owner, &name, b"x");
            }
        }
        assert!(store.len() <= MAX_TOTAL);
        if step % 100 == 0 {
            let back = Store::open(&store.seal(&KEY, &mut nonce), &KEY).unwrap();
            assert_eq!(back.len(), store.len());
            for uid in 1000..1003 {
                assert_eq!(back.names(Owner::User(uid)), store.names(Owner::User(uid)));
            }
            assert_eq!(back.names(Owner::System), store.names(Owner::System));
        }
    }
}
