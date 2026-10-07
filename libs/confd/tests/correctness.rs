//! Path, limit, access and listing behaviour of the store.

mod common;

use common::{blob, caller, text, ALICE, BOB, ROOT};
use confd::{
    validate_path, Change, Error, Store, Value, MAX_PATH_LEN, MAX_STORE_BYTES, MAX_VALUE_LEN,
};

#[test]
fn accepts_valid_paths() {
    for path in [
        "sys",
        "sys/net/eth0/mtu",
        "user/1000",
        "user/1000/shell/theme",
        "sys/a.b-c_1",
        "user/0/x",
        "user/4294967295",
        // `.` is in the alphabet; only the `..` segment is special.
        "sys/.",
        "sys/..x",
        "sys/-",
    ] {
        assert_eq!(validate_path(path), Ok(()), "{path}");
    }

    let at_limit = format!("sys/{}", "a".repeat(MAX_PATH_LEN - 4));
    assert_eq!(at_limit.len(), MAX_PATH_LEN);
    assert_eq!(validate_path(&at_limit), Ok(()));

    let over_limit = format!("sys/{}", "a".repeat(MAX_PATH_LEN - 3));
    assert_eq!(over_limit.len(), MAX_PATH_LEN + 1);
    assert_eq!(validate_path(&over_limit), Err(Error::BadPath));
}

#[test]
fn rejects_malformed_paths() {
    for path in [
        "",
        "/",
        "/sys",
        "sys/",
        "user/",
        "sys//x",
        "sys/x/",
        "SYS",
        "Sys",
        "system",
        "sysx",
        "usr/x",
        "sys/Net",
        "sys/net!",
        "sys/net x",
        "sys/net\\x",
        "sys/tab\there",
        "sys/new\nline",
        "sys/..",
        "sys/../x",
        "sys/a/../../b",
        "user/..",
        "sys/n\u{e9}",
        "user/1000/\u{1f600}",
    ] {
        assert_eq!(validate_path(path), Err(Error::BadPath), "{path:?}");
    }
}

#[test]
fn value_size_limit_is_enforced_at_the_boundary() {
    let mut store = Store::new();

    let at_limit = Value::Str("x".repeat(MAX_VALUE_LEN));
    store.set("sys/str", at_limit.clone(), ROOT).unwrap();
    assert_eq!(store.get("sys/str", ROOT).unwrap(), Some(&at_limit));

    let over_limit = Value::Str("x".repeat(MAX_VALUE_LEN + 1));
    assert_eq!(store.set("sys/big", over_limit, ROOT), Err(Error::TooLarge));
    assert_eq!(store.get("sys/big", ROOT).unwrap(), None);

    assert_eq!(
        store.set("sys/blob", blob(&vec![0; MAX_VALUE_LEN + 1]), ROOT),
        Err(Error::TooLarge)
    );
    store
        .set("sys/blob", blob(&vec![7; MAX_VALUE_LEN]), ROOT)
        .unwrap();

    // Fixed-size variants are always inside the value limit.
    store.set("sys/u", Value::U64(u64::MAX), ROOT).unwrap();
    store.set("sys/i", Value::I64(i64::MIN), ROOT).unwrap();
    store.set("sys/b", Value::Bool(false), ROOT).unwrap();
}

#[test]
fn store_size_limit_is_enforced_exactly() {
    let mut store = Store::new();
    let mut used = 0usize;
    let mut index = 0usize;

    // Fill the store to exactly MAX_STORE_BYTES with distinct paths.
    while MAX_STORE_BYTES - used >= 10 {
        let path = format!("sys/p{index:05}");
        let path_len = path.len();
        let value_len = (MAX_STORE_BYTES - used - path_len).min(MAX_VALUE_LEN);
        store
            .set(&path, blob(&vec![index as u8; value_len]), ROOT)
            .unwrap();
        used += path_len + value_len;
        index += 1;
    }
    assert_eq!(used, MAX_STORE_BYTES);

    let next = format!("sys/p{index:05}");
    assert_eq!(
        store.set(&next, Value::Bool(true), ROOT),
        Err(Error::TooLarge)
    );
    assert_eq!(store.get(&next, ROOT).unwrap(), None);

    // Shrinking an existing entry frees exactly that much room again.
    store
        .set("sys/p00000", Value::Bytes(Vec::new()), ROOT)
        .unwrap();
    assert!(store.set(&next, Value::Bool(true), ROOT).is_ok());
}

#[test]
fn failed_set_preserves_the_previous_value() {
    let mut store = Store::new();
    store.set("sys/key", text("old"), ROOT).unwrap();
    assert_eq!(
        store.set("sys/key", Value::Str("x".repeat(MAX_VALUE_LEN + 1)), ROOT),
        Err(Error::TooLarge)
    );
    assert_eq!(store.get("sys/key", ROOT).unwrap(), Some(&text("old")));
}

#[test]
fn sys_is_world_readable_and_root_writable() {
    let mut store = Store::new();
    store.set("sys/net/mtu", Value::U64(1500), ROOT).unwrap();

    assert_eq!(
        store.get("sys/net/mtu", ALICE).unwrap(),
        Some(&Value::U64(1500))
    );
    assert_eq!(
        store.set("sys/net/mtu", Value::U64(9000), ALICE),
        Err(Error::Denied)
    );
    assert_eq!(store.delete("sys/net/mtu", BOB), Err(Error::Denied));
    assert!(store.set("sys/net/mtu", Value::U64(9000), ROOT).is_ok());
}

#[test]
fn user_subtrees_are_owner_or_root_only() {
    let mut store = Store::new();
    store
        .set("user/1000/shell/theme", text("dark"), ALICE)
        .unwrap();

    assert!(store.get("user/1000/shell/theme", ALICE).is_ok());
    assert!(store.get("user/1000/shell/theme", ROOT).is_ok());
    assert_eq!(store.get("user/1000/shell/theme", BOB), Err(Error::Denied));
    assert_eq!(
        store.set("user/1000/x", Value::Bool(true), BOB),
        Err(Error::Denied)
    );
    assert_eq!(
        store.delete("user/1000/shell/theme", BOB),
        Err(Error::Denied)
    );
    assert!(store.get("user/1000/shell/theme", ROOT).is_ok());
}

#[test]
fn access_is_checked_before_size() {
    let mut store = Store::new();
    let too_large = Value::Str("x".repeat(MAX_VALUE_LEN + 1));
    // Alice cannot write sys at all, so she sees Denied, not TooLarge.
    assert_eq!(store.set("sys/a", too_large, ALICE), Err(Error::Denied));
}

#[test]
fn owner_segment_is_parsed_as_a_number() {
    let mut store = Store::new();
    store.set("user/1000/a", Value::Bool(true), ALICE).unwrap();
    // A differently spelled owner is a different key owned by the same uid.
    assert_eq!(store.get("user/01000/a", ALICE).unwrap(), None);
    assert_eq!(store.get("user/01000/a", BOB), Err(Error::Denied));
}

#[test]
fn denial_is_not_absence() {
    let mut store = Store::new();
    store.set("user/1001/notes", text("private"), BOB).unwrap();

    // Alice may not learn whether Bob's path exists.
    assert_eq!(store.get("user/1001/notes", ALICE), Err(Error::Denied));
    assert_eq!(store.get("user/1001/missing", ALICE), Err(Error::Denied));
    // Root sees the difference.
    assert!(store.get("user/1001/notes", ROOT).is_ok());
    assert_eq!(store.get("user/1001/missing", ROOT).unwrap(), None);

    // Deleting an absent but denied path is denied, not Ok.
    assert_eq!(store.delete("user/1001/missing", ALICE), Err(Error::Denied));
}

#[test]
fn unclaimed_user_paths_are_denied_to_everyone() {
    let mut store = Store::new();
    for caller in [ROOT, ALICE, BOB] {
        assert_eq!(store.get("user", caller), Err(Error::Denied));
        assert_eq!(store.get("user/alice/x", caller), Err(Error::Denied));
        assert_eq!(store.get("user/-1/x", caller), Err(Error::Denied));
        assert_eq!(store.get("user/4294967296/x", caller), Err(Error::Denied));
        assert_eq!(
            store.set("user/alice/x", Value::Bool(true), caller),
            Err(Error::Denied)
        );
        assert_eq!(store.delete("user/alice/x", caller), Err(Error::Denied));
    }
    assert!(store.is_empty());
}

#[test]
fn bad_path_wins_over_denial() {
    let store = Store::new();
    assert_eq!(store.get("usr/1000", ALICE), Err(Error::BadPath));
    assert_eq!(store.get("", ALICE), Err(Error::BadPath));
}

#[test]
fn set_returns_the_change_to_publish() {
    let mut store = Store::new();
    let change = store.set("sys/a", Value::Bool(true), ROOT).unwrap();
    assert_eq!(
        change,
        Change {
            path: String::from("sys/a"),
            new: Some(Value::Bool(true)),
        }
    );

    let overwrite = store.set("sys/a", Value::U64(5), ROOT).unwrap();
    assert_eq!(overwrite.new, Some(Value::U64(5)));
}

#[test]
fn delete_absent_is_ok_and_absent_from_changes() {
    let mut store = Store::new();
    assert_eq!(store.delete("sys/nope", ROOT).unwrap(), None);

    store.set("sys/a", Value::Bool(true), ROOT).unwrap();
    let change = store.delete("sys/a", ROOT).unwrap().unwrap();
    assert_eq!(change.path, "sys/a");
    assert_eq!(change.new, None);
    assert_eq!(store.get("sys/a", ROOT).unwrap(), None);
    assert!(store.is_empty());
}

#[test]
fn list_filters_by_access_and_segment_prefix() {
    let mut store = Store::new();
    for path in [
        "sys/a",
        "sys/net/x",
        "sys/netx",
        "user/1000/a",
        "user/1000/sub/b",
        "user/1001/a",
        "user/42/a",
    ] {
        store.set(path, Value::Bool(true), ROOT).unwrap();
    }

    assert_eq!(
        store.list("", ALICE).unwrap(),
        vec![
            "sys/a",
            "sys/net/x",
            "sys/netx",
            "user/1000/a",
            "user/1000/sub/b"
        ]
    );
    assert_eq!(
        store.list("", ROOT).unwrap(),
        vec![
            "sys/a",
            "sys/net/x",
            "sys/netx",
            "user/1000/a",
            "user/1000/sub/b",
            "user/1001/a",
            "user/42/a",
        ]
    );
    assert_eq!(
        store.list("sys", ALICE).unwrap(),
        vec!["sys/a", "sys/net/x", "sys/netx"]
    );
    // Segment-aware: sys/net must not list sys/netx.
    assert_eq!(store.list("sys/net", ROOT).unwrap(), vec!["sys/net/x"]);
    // The exact path is included when it holds a value.
    assert_eq!(store.list("sys/a", ROOT).unwrap(), vec!["sys/a"]);
    assert_eq!(
        store.list("user", ALICE).unwrap(),
        vec!["user/1000/a", "user/1000/sub/b"]
    );
    // Bob learns nothing about Alice's subtree.
    assert_eq!(store.list("user/1000", BOB).unwrap(), Vec::<&str>::new());
    assert_eq!(store.list("user/1001", BOB).unwrap(), vec!["user/1001/a"]);

    assert_eq!(store.list("nope", ROOT), Err(Error::BadPath));
    assert_eq!(store.list("sys/", ROOT), Err(Error::BadPath));
    assert_eq!(store.list("sys//x", ROOT), Err(Error::BadPath));
}

#[test]
fn caller_field_constructs_directly() {
    assert_eq!(
        caller(7),
        confd::Caller {
            uid: 7,
            system: false
        }
    );
}
