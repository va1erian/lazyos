//! Apps' secrets: scoping, limits, the versioned file, the sweep.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::*;

const KEY: [u8; MACHINE_KEY_LEN] = [9; MACHINE_KEY_LEN];

fn counter() -> impl FnMut(&mut [u8; 16]) {
    let mut next = 0u64;
    move |nonce| {
        nonce[..8].copy_from_slice(&next.to_le_bytes());
        next += 1;
    }
}

fn sample() -> AppStore {
    let mut store = AppStore::new();
    store.put(1000, "os.lazy.mail", "imap", b"hunter2").unwrap();
    store.put(1000, "os.lazy.mail", "smtp", b"hunter3").unwrap();
    store.put(1001, "os.lazy.mail", "imap", b"other user").unwrap();
    store.put(1000, "org.lazy.agent", "key", &[0xAB; 1024]).unwrap();
    store
}

// ---- who is an app -------------------------------------------------------

#[test]
fn only_an_installed_app_label_names_an_app() {
    assert_eq!(app_of_label("app:os.lazy.mail"), Some("os.lazy.mail"));
    assert_eq!(app_of_label("app:a"), Some("a"));
    for refused in [
        "",
        "app:",
        "dev:os.lazy.mail",
        "app:os.lazy.mail/child",
        "app:os.lazy.mail/",
        "app:a b",
        "app:a\0b",
        "app:../x",
        "App:os.lazy.mail",
        " app:os.lazy.mail",
        "os.lazy.mail",
    ] {
        assert_eq!(app_of_label(refused), None, "{refused:?}");
    }
    assert!(app_of_label(&format!("app:{}", "a".repeat(APP_MAX_ID))).is_some());
    assert!(app_of_label(&format!("app:{}", "a".repeat(APP_MAX_ID + 1))).is_none());
}

// ---- the table -----------------------------------------------------------

#[test]
fn a_secret_reads_back_only_for_its_user_and_app() {
    let store = sample();
    assert_eq!(store.get(1000, "os.lazy.mail", "imap"), Ok(&b"hunter2"[..]));
    assert_eq!(store.get(1001, "os.lazy.mail", "imap"), Ok(&b"other user"[..]));
    // Another app, another user, another name: nothing.
    assert_eq!(store.get(1000, "org.lazy.agent", "imap"), Err(AppError::NotFound));
    assert_eq!(store.get(1002, "os.lazy.mail", "imap"), Err(AppError::NotFound));
    assert_eq!(store.get(1000, "os.lazy.mail", "key"), Err(AppError::NotFound));
    // A prefix of an app's name is not that app.
    assert_eq!(store.get(1000, "os.lazy", "imap"), Err(AppError::NotFound));
    assert_eq!(store.names(1000, "os.lazy.mail"), ["imap", "smtp"]);
    assert_eq!(store.names(1001, "os.lazy.mail"), ["imap"]);
    assert!(store.names(1002, "os.lazy.mail").is_empty());
}

#[test]
fn put_replaces_and_delete_forgets() {
    let mut store = sample();
    store.put(1000, "os.lazy.mail", "imap", b"new").unwrap();
    assert_eq!(store.get(1000, "os.lazy.mail", "imap"), Ok(&b"new"[..]));
    assert_eq!(store.len(), 4, "replacing adds no row");
    store.delete(1000, "os.lazy.mail", "imap").unwrap();
    assert_eq!(store.get(1000, "os.lazy.mail", "imap"), Err(AppError::NotFound));
    assert_eq!(store.delete(1000, "os.lazy.mail", "imap"), Err(AppError::NotFound));
    // Another user's secret of the same name stayed.
    assert!(store.get(1001, "os.lazy.mail", "imap").is_ok());
}

#[test]
fn names_and_secrets_are_bounded() {
    let mut store = AppStore::new();
    let long = "n".repeat(APP_MAX_NAME);
    assert!(store.put(1, "a", &long, b"x").is_ok());
    assert_eq!(store.put(1, "a", &format!("{long}n"), b"x"), Err(AppError::BadName));
    for bad in ["", "a b", "tab\t", "new\nline", "nul\0", "caf\u{e9}"] {
        assert_eq!(store.put(1, "a", bad, b"x"), Err(AppError::BadName), "{bad:?}");
    }
    assert!(store.put(1, "a", "user@host:993", b"x").is_ok(), "printable ASCII is fine");
    assert!(store.put(1, "a", "big", &[1; APP_MAX_SECRET]).is_ok());
    assert_eq!(
        store.put(1, "a", "big", &[1; APP_MAX_SECRET + 1]),
        Err(AppError::BadSecret)
    );
    assert_eq!(store.put(1, "a", "empty", b""), Err(AppError::BadSecret));
    assert_eq!(APP_MAX_SECRET, 1024);
    assert_eq!(APP_MAX_NAME, 64);
    assert_eq!(APP_MAX_PER_APP, 16);
}

#[test]
fn an_app_holds_sixteen_per_user_and_the_table_is_capped() {
    let mut store = AppStore::new();
    for n in 0..APP_MAX_PER_APP {
        store.put(1, "a", &format!("k{n}"), b"x").unwrap();
    }
    assert_eq!(store.put(1, "a", "one-more", b"x"), Err(AppError::Full));
    // Replacing one of the sixteen still works, and so do other users and apps.
    assert!(store.put(1, "a", "k0", b"y").is_ok());
    assert!(store.put(2, "a", "k0", b"y").is_ok());
    assert!(store.put(1, "b", "k0", b"y").is_ok());
    // The table's own cap: fill with distinct (uid, app) pairs.
    let mut full = AppStore::new();
    let mut uid = 0;
    while full.len() < APP_MAX_TOTAL {
        full.put(uid, "a", "k", b"x").unwrap();
        uid += 1;
    }
    assert_eq!(full.put(uid, "a", "k", b"x"), Err(AppError::Full));
    assert!(full.put(0, "a", "k", b"z").is_ok(), "a replacement needs no room");
}

// ---- the sweep -----------------------------------------------------------

#[test]
fn a_sweep_deletes_the_secrets_of_apps_no_longer_installed() {
    let mut store = sample();
    let mut asked: Vec<String> = Vec::new();
    let removed = store.sweep(|app| {
        asked.push(app.into());
        app == "os.lazy.mail"
    });
    assert_eq!(removed, 1);
    assert_eq!(asked, ["org.lazy.agent", "os.lazy.mail"], "each app is asked once");
    assert_eq!(store.get(1000, "org.lazy.agent", "key"), Err(AppError::NotFound));
    assert_eq!(store.len(), 3, "the installed app's secrets stayed, for every user");
    assert_eq!(store.sweep(|_| true), 0);
    assert_eq!(store.sweep(|_| false), 3);
    assert!(store.is_empty());
}

#[test]
fn an_app_is_swept_only_when_missing_twice_in_a_row() {
    let s = |names: &[&str]| -> Vec<String> { names.iter().map(|n| (*n).into()).collect() };
    assert!(confirmed_gone(&s(&[]), &s(&["a", "b"])).is_empty(), "first miss");
    assert_eq!(confirmed_gone(&s(&["a", "b"]), &s(&["b"])), s(&["b"]));
    assert!(confirmed_gone(&s(&["a"]), &s(&[])).is_empty(), "it came back");
    assert!(confirmed_gone(&s(&["a"]), &s(&["c"])).is_empty());
}

// ---- the file ------------------------------------------------------------

#[test]
fn a_file_round_trips_under_a_version_header() {
    let store = sample();
    let bytes = store.seal(&KEY, &mut counter());
    assert!(bytes.len() <= APP_FILE_MAX);
    assert_eq!(&bytes[..8], b"LZAPSECR");
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), APP_FILE_VERSION);
    let back = AppStore::open(&bytes, &KEY).unwrap();
    assert_eq!(back.len(), store.len());
    assert_eq!(back.get(1000, "os.lazy.mail", "imap"), Ok(&b"hunter2"[..]));
    assert_eq!(back.get(1001, "os.lazy.mail", "imap"), Ok(&b"other user"[..]));
    assert_eq!(back.get(1000, "org.lazy.agent", "key"), Ok(&[0xAB; 1024][..]));
    assert!(AppStore::open(&AppStore::new().seal(&KEY, &mut counter()), &KEY)
        .unwrap()
        .is_empty());
}

#[test]
fn the_file_holds_no_plaintext() {
    let bytes = sample().seal(&KEY, &mut counter());
    for needle in [&b"hunter2"[..], b"os.lazy.mail", b"imap", b"other user"] {
        assert!(!bytes.windows(needle.len()).any(|w| w == needle));
    }
}

#[test]
fn an_unknown_version_is_refused_whole() {
    let mut bytes = sample().seal(&KEY, &mut counter());
    bytes[8] = 2;
    // The version is covered by the tag, so a bare edit is damage...
    assert_eq!(AppStore::open(&bytes, &KEY).err(), Some(FileError::BadTag));
    // ...and a writer that really made version 2 is told it is too new.
    let tag = lazyos_crypto::hmac::hmac_sha256(&KEY, b"lazyos keyd appsecrets file");
    let n = bytes.len() - 32;
    let retag = lazyos_crypto::hmac::hmac_sha256(&tag, &bytes[..n]);
    bytes[n..].copy_from_slice(&retag);
    assert_eq!(AppStore::open(&bytes, &KEY).err(), Some(FileError::BadVersion));
}

#[test]
fn damage_and_the_wrong_key_are_refused() {
    let bytes = sample().seal(&KEY, &mut counter());
    assert_eq!(AppStore::open(&bytes, &[1; MACHINE_KEY_LEN]).err(), Some(FileError::BadTag));
    assert_eq!(AppStore::open(&bytes[..9], &KEY).err(), Some(FileError::Truncated));
    assert_eq!(AppStore::open(&[], &KEY).err(), Some(FileError::Truncated));
    for at in [0, 9, 12, 40, bytes.len() - 1] {
        let mut flipped = bytes.clone();
        flipped[at] ^= 1;
        assert!(AppStore::open(&flipped, &KEY).is_err(), "byte {at}");
    }
    // The named secrets' file is another format, not this one.
    let mut named = Store::new();
    named.put(Owner::User(1), "x", b"y").unwrap();
    let named = named.seal(&KEY, &mut counter());
    assert_eq!(AppStore::open(&named, &KEY).err(), Some(FileError::BadMagic));
    assert_eq!(Store::open(&bytes, &KEY).err(), Some(FileError::BadMagic));
}

#[test]
fn a_nonce_is_fresh_per_record_and_per_seal() {
    let mut store = AppStore::new();
    store.put(1, "a", "x", b"same").unwrap();
    store.put(1, "a", "y", b"same").unwrap();
    let mut nonce = counter();
    let one = store.seal(&KEY, &mut nonce);
    let two = store.seal(&KEY, &mut nonce);
    assert_ne!(one, two);
}

#[test]
fn a_soak_of_changes_stays_consistent_through_the_file() {
    let mut store = AppStore::new();
    let mut nonce = counter();
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for step in 0..3000 {
        let uid = (next() % 3) as u32;
        let app = format!("app{}", next() % 4);
        let name = format!("k{}", next() % 20);
        match next() % 4 {
            0 | 1 => {
                let secret = alloc::vec![(next() & 0xff) as u8; 1 + (next() % 1024) as usize];
                let _ = store.put(uid, &app, &name, &secret);
            }
            2 => {
                let _ = store.delete(uid, &app, &name);
            }
            _ => {
                let gone = format!("app{}", next() % 4);
                store.sweep(|a| a != gone);
            }
        }
        if step % 100 == 0 {
            let back = AppStore::open(&store.seal(&KEY, &mut nonce), &KEY).unwrap();
            assert_eq!(back.len(), store.len());
            for uid in 0..3 {
                for app in store.apps() {
                    assert_eq!(back.names(uid, &app), store.names(uid, &app));
                    for name in store.names(uid, &app) {
                        assert_eq!(back.get(uid, &app, &name), store.get(uid, &app, &name));
                    }
                }
            }
        }
    }
}
