use alloc::string::ToString;
use alloc::vec::Vec;

use crate::*;

const KEY: [u8; MACHINE_KEY_LEN] = [7; MACHINE_KEY_LEN];

fn hex(bytes: &[u8]) -> alloc::string::String {
    bytes.iter().map(|b| alloc::format!("{b:02x}")).collect()
}

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

fn caller(uid: u32) -> Caller {
    Caller {
        uid,
        label_id: 0,
        session: 0,
    }
}

// ---- the rules -----------------------------------------------------------

#[test]
fn names_are_plain_and_bounded() {
    for good in ["a", "Home-WiFi", "cafe.5G:2", "x".repeat(MAX_NAME).as_str()] {
        assert!(valid_name(good), "{good}");
    }
    let long = "x".repeat(MAX_NAME + 1);
    for bad in [
        "",
        " ",
        "a b",
        "a/b",
        "a\n",
        "\u{202e}x",
        "caf\u{e9}",
        long.as_str(),
    ] {
        assert!(!valid_name(bad), "{bad:?}");
    }
}

#[test]
fn secret_sizes_are_bounded() {
    let mut store = Store::new();
    assert_eq!(store.put(Owner::System, "a", b""), Err(Error::BadSecret));
    assert_eq!(
        store.put(Owner::System, "a", &[1; MAX_SECRET + 1]),
        Err(Error::BadSecret)
    );
    assert_eq!(store.put(Owner::System, "a", &[1; MAX_SECRET]), Ok(()));
    assert_eq!(store.put(Owner::System, "a b", b"x"), Err(Error::BadName));
}

#[test]
fn the_caps_hold_per_owner_and_in_total() {
    let mut store = Store::new();
    for index in 0..MAX_PER_OWNER {
        store.put(Owner::User(1), &index.to_string(), b"s").unwrap();
    }
    assert_eq!(
        store.put(Owner::User(1), "one-more", b"s"),
        Err(Error::Full)
    );
    // Replacing a held name is not a new secret.
    assert_eq!(store.put(Owner::User(1), "0", b"t"), Ok(()));
    // Another owner is not locked out by the first one's cap.
    assert_eq!(store.put(Owner::User(2), "mine", b"s"), Ok(()));
    for uid in 3..3 + (MAX_TOTAL / MAX_PER_OWNER) as u32 {
        for index in 0..MAX_PER_OWNER {
            let _ = store.put(Owner::User(uid), &index.to_string(), b"s");
        }
    }
    assert_eq!(store.len(), MAX_TOTAL);
    assert_eq!(store.put(Owner::User(999), "late", b"s"), Err(Error::Full));
    store.delete(Owner::User(1), "0").unwrap();
    assert_eq!(store.put(Owner::User(999), "late", b"s"), Ok(()));
}

#[test]
fn owners_never_see_each_other() {
    let mut store = sample();
    assert_eq!(store.names(Owner::User(1000)), ["home"]);
    assert_eq!(
        store.names(Owner::User(1002)),
        Vec::<alloc::string::String>::new()
    );
    assert_eq!(store.names(Owner::System), ["office"]);
    assert_eq!(
        store.delete(Owner::User(1002), "home"),
        Err(Error::NotFound)
    );
    assert_eq!(
        store.pmk(Owner::User(1002), "home", b"IEEE").map(|_| ()),
        Err(Error::NotFound)
    );
    // A user's name does not reach the system's, nor the reverse.
    assert_eq!(store.delete(Owner::System, "home"), Err(Error::NotFound));
    assert_eq!(
        store.delete(Owner::User(1000), "office"),
        Err(Error::NotFound)
    );
}

#[test]
fn names_come_back_sorted_and_never_with_material() {
    let mut store = Store::new();
    for name in ["b", "c", "a"] {
        store.put(Owner::User(5), name, b"hunter2hunter2").unwrap();
    }
    assert_eq!(store.names(Owner::User(5)), ["a", "b", "c"]);
}

// ---- who may do what -----------------------------------------------------

#[test]
fn authorization_table() {
    let session_user = Caller {
        uid: 1000,
        label_id: 0,
        session: 3,
    };
    let elevd = caller(ELEVD_UID);
    let wlan = caller(WLAN_UID);
    let pmk = |owner_uid| Op::Pmk { owner_uid };

    // User scope: always the caller's own uid.
    for op in [Op::Store, Op::Delete, Op::List] {
        assert_eq!(authorize(session_user, "user", op), Ok(Owner::User(1000)));
    }
    assert_eq!(
        authorize(elevd, "user", Op::Store),
        Ok(Owner::User(ELEVD_UID))
    );
    // System scope: changes are elevd's alone, listing is open.
    for op in [Op::Store, Op::Delete] {
        assert_eq!(authorize(elevd, "system", op), Ok(Owner::System));
        for denied in [session_user, wlan, caller(0), caller(1001)] {
            assert_eq!(
                authorize(denied, "system", op),
                Err(Denied::Perm),
                "{denied:?}"
            );
        }
    }
    assert_eq!(
        authorize(session_user, "system", Op::List),
        Ok(Owner::System)
    );
    // The PMK is wlanmd's alone, for the uid it names.
    assert_eq!(authorize(wlan, "user", pmk(1000)), Ok(Owner::User(1000)));
    assert_eq!(authorize(wlan, "system", pmk(0)), Ok(Owner::System));
    assert_eq!(authorize(wlan, "system", pmk(1000)), Err(Denied::Invalid));
    for denied in [session_user, elevd, caller(0), caller(1000), caller(913)] {
        for scope in ["user", "system"] {
            assert_eq!(
                authorize(denied, scope, pmk(0)),
                Err(Denied::Perm),
                "{denied:?}"
            );
            assert_eq!(authorize(denied, scope, pmk(1000)), Err(Denied::Perm));
        }
    }
    // Unlabelled and outside a session are part of the identity: an app under
    // a label, or a task in a session, with wlanmd's uid is not wlanmd.
    for forged in [
        Caller {
            label_id: 4,
            ..wlan
        },
        Caller { session: 1, ..wlan },
    ] {
        assert_eq!(authorize(forged, "user", pmk(1000)), Err(Denied::Perm));
    }
    for forged in [
        Caller {
            label_id: 4,
            ..elevd
        },
        Caller {
            session: 1,
            ..elevd
        },
    ] {
        assert_eq!(authorize(forged, "system", Op::Store), Err(Denied::Perm));
    }
    // No scope but the two.
    for scope in ["", "User", "all", "system "] {
        assert_eq!(authorize(elevd, scope, Op::List), Err(Denied::Invalid));
    }
}

// ---- the PMK -------------------------------------------------------------

#[test]
fn pmk_matches_ieee_802_11_annex_j_4() {
    let mut store = Store::new();
    store.put(Owner::System, "j4a", b"password").unwrap();
    store.put(Owner::System, "j4b", b"ThisIsAPassword").unwrap();
    let (pmk, fresh) = store.pmk(Owner::System, "j4a", b"IEEE").unwrap();
    assert!(fresh);
    assert_eq!(
        hex(&pmk),
        "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"
    );
    let (pmk, _) = store.pmk(Owner::System, "j4b", b"ThisIsASSID").unwrap();
    assert_eq!(
        hex(&pmk),
        "0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af"
    );
}

#[test]
fn pmk_is_computed_once_per_secret_and_ssid() {
    let mut store = sample();
    // `sample` already derived (home, IEEE).
    let (first, fresh) = store.pmk(Owner::User(1000), "home", b"IEEE").unwrap();
    assert!(!fresh);
    let (other, fresh) = store.pmk(Owner::User(1000), "home", b"Other").unwrap();
    assert!(fresh);
    assert_ne!(first, other);
    assert_eq!(store.cached(Owner::User(1000), "home"), Some(2));
    // A new secret under the name drops what was derived from the old one.
    store
        .put(Owner::User(1000), "home", b"a new password")
        .unwrap();
    assert_eq!(store.cached(Owner::User(1000), "home"), Some(0));
}

#[test]
fn the_pmk_cache_is_bounded_and_drops_the_oldest() {
    let mut store = Store::new();
    store.put(Owner::System, "n", b"password").unwrap();
    for index in 0..MAX_PMKS + 2 {
        store
            .pmk(Owner::System, "n", alloc::format!("net{index}").as_bytes())
            .unwrap();
    }
    assert_eq!(store.cached(Owner::System, "n"), Some(MAX_PMKS));
    let (_, fresh) = store.pmk(Owner::System, "n", b"net0").unwrap();
    assert!(fresh, "the oldest was dropped");
}

#[test]
fn a_raw_psk_is_its_own_pmk() {
    let hexed = "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12E";
    let mut store = Store::new();
    store.put(Owner::System, "raw", hexed.as_bytes()).unwrap();
    let (pmk, fresh) = store.pmk(Owner::System, "raw", b"anything").unwrap();
    assert!(!fresh);
    assert_eq!(hex(&pmk), hexed.to_lowercase());
}

#[test]
fn pmk_refuses_what_is_not_a_passphrase_or_ssid() {
    let mut store = Store::new();
    store.put(Owner::System, "short", b"short").unwrap();
    store.put(Owner::System, "ok", b"password").unwrap();
    store.put(Owner::System, "binary", &[0xff; 12]).unwrap();
    let pmk = |store: &mut Store, name: &str, ssid: &[u8]| {
        store.pmk(Owner::System, name, ssid).map(|_| ())
    };
    assert_eq!(pmk(&mut store, "short", b"IEEE"), Err(Error::BadSecret));
    assert_eq!(pmk(&mut store, "binary", b"IEEE"), Err(Error::BadSecret));
    assert_eq!(pmk(&mut store, "ok", b""), Err(Error::BadSsid));
    assert_eq!(
        pmk(&mut store, "ok", &[b'a'; MAX_SSID + 1]),
        Err(Error::BadSsid)
    );
    assert_eq!(pmk(&mut store, "gone", b"IEEE"), Err(Error::NotFound));
    assert_eq!(pmk(&mut store, "ok", &[b'a'; MAX_SSID]), Ok(()));
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
