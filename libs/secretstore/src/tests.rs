use alloc::string::ToString;
use alloc::vec::Vec;

use crate::*;

fn hex(bytes: &[u8]) -> alloc::string::String {
    bytes.iter().map(|b| alloc::format!("{b:02x}")).collect()
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
