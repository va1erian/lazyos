//! Service-core behaviour: uid checks, persist-before-swap, change filtering.

mod common;

use common::{text, MemoryFs, ALICE, BOB, ROOT};
use regd::service::announceable;
use regd::{ChangeSink, Regd, ServiceError, Value};

/// A sink that records every announcement for inspection.
#[derive(Default)]
struct RecordingSink {
    events: Vec<(String, bool)>,
}

impl ChangeSink for RecordingSink {
    fn changed(&mut self, path: &str, deleted: bool) {
        self.events.push((String::from(path), deleted));
    }
}

/// The announcements a test service recorded, cloned out through its sink.
fn events(regd: &Regd<MemoryFs, RecordingSink>) -> Vec<(String, bool)> {
    regd.sink().events.clone()
}

fn service() -> Regd<MemoryFs, RecordingSink> {
    Regd::load(MemoryFs::new(), RecordingSink::default()).expect("empty store loads")
}

fn push(value: &str) -> Value {
    text(value)
}

#[test]
fn set_get_list_delete_round_trip() {
    let mut regd = service();
    assert_eq!(regd.get("sys/net/mtu", ROOT).unwrap(), None);

    regd.set("sys/net/mtu", Value::U64(1500), ROOT).unwrap();
    regd.set("sys/net/name", push("eth0"), ROOT).unwrap();
    assert_eq!(
        regd.get("sys/net/mtu", ROOT).unwrap(),
        Some(&Value::U64(1500))
    );
    assert_eq!(
        regd.list("sys/net", ROOT).unwrap(),
        vec!["sys/net/mtu", "sys/net/name"]
    );

    regd.delete("sys/net/mtu", ROOT).unwrap();
    assert_eq!(regd.get("sys/net/mtu", ROOT).unwrap(), None);
    // Deleting an absent path is a no-op, not an error.
    regd.delete("sys/net/mtu", ROOT).unwrap();
}

#[test]
fn access_rules_are_enforced() {
    let mut regd = service();
    // A non-root caller may not write the system subtree.
    assert_eq!(
        regd.set("sys/net/mtu", Value::U64(1), ALICE),
        Err(ServiceError::Denied)
    );
    assert_eq!(regd.get("sys/net/mtu", ALICE).unwrap(), None);

    // A user may read and write its own subtree...
    regd.set("user/1000/theme", push("dark"), ALICE).unwrap();
    assert_eq!(
        regd.get("user/1000/theme", ALICE).unwrap(),
        Some(&push("dark"))
    );
    // ... but a different user may not, and cannot probe its absence.
    assert_eq!(regd.get("user/1000/theme", BOB), Err(ServiceError::Denied));
    assert_eq!(
        regd.set("user/1000/theme", push("light"), BOB),
        Err(ServiceError::Denied)
    );
    // Root may access any user subtree.
    assert_eq!(
        regd.get("user/1000/theme", ROOT).unwrap(),
        Some(&push("dark"))
    );

    // A path outside the two subtrees is invalid.
    assert_eq!(regd.get("etc/passwd", ROOT), Err(ServiceError::BadPath));
}

#[test]
fn list_filters_other_users_paths() {
    let mut regd = service();
    regd.set("user/1000/a", Value::Bool(true), ALICE).unwrap();
    regd.set("user/1001/b", Value::Bool(true), BOB).unwrap();
    regd.set("sys/a", Value::Bool(true), ROOT).unwrap();

    assert_eq!(regd.list("", ALICE).unwrap(), vec!["sys/a", "user/1000/a"]);
    assert_eq!(regd.list("user", ALICE).unwrap(), vec!["user/1000/a"]);
}

#[test]
fn committed_changes_are_announced_only_for_sys() {
    let mut regd = service();
    regd.set("sys/net/mtu", Value::U64(1500), ROOT).unwrap();
    regd.set("user/1000/theme", push("dark"), ALICE).unwrap();
    regd.delete("sys/net/mtu", ROOT).unwrap();

    // The `user/` write committed but was intentionally not announced; only
    // the two `sys/` transitions are visible.
    assert_eq!(
        events(&regd),
        vec![
            (String::from("sys/net/mtu"), false),
            (String::from("sys/net/mtu"), true),
        ]
    );
}

#[test]
fn announceable_selects_the_system_subtree() {
    assert!(announceable("sys"));
    assert!(announceable("sys/net/mtu"));
    assert!(!announceable("systolic"));
    assert!(!announceable("user/1000/theme"));
    assert!(!announceable("user"));
}

#[test]
fn persist_failure_leaves_the_live_store_untouched() {
    // Seed a committed store, then copy the filesystem before injecting the
    // failure so the untouched copy can be compared.
    let mut seeded = Regd::load(MemoryFs::new(), RecordingSink::default()).unwrap();
    seeded.set("sys/keep", Value::U64(1), ROOT).unwrap();
    let healthy = seeded.fs().clone();

    let mut broken_fs = healthy.clone();
    broken_fs.fail_writes();
    let mut broken = Regd::load(broken_fs, RecordingSink::default()).unwrap();
    assert_eq!(
        broken.set("sys/new", Value::U64(2), ROOT),
        Err(ServiceError::Io)
    );
    // The value was not committed and nothing was announced.
    assert_eq!(broken.get("sys/new", ROOT).unwrap(), None);
    assert_eq!(broken.get("sys/keep", ROOT).unwrap(), Some(&Value::U64(1)));
    assert!(events(&broken).is_empty());

    // The untouched copy still loads the old store.
    let intact = Regd::load(healthy, RecordingSink::default()).unwrap();
    assert_eq!(intact.get("sys/new", ROOT).unwrap(), None);
    assert_eq!(intact.get("sys/keep", ROOT).unwrap(), Some(&Value::U64(1)));
}

#[test]
fn restart_reloads_the_same_data() {
    let mut first = Regd::load(MemoryFs::new(), RecordingSink::default()).unwrap();
    first.set("sys/a", Value::I64(-1), ROOT).unwrap();
    first.set("user/1000/b", push("kept"), ALICE).unwrap();

    let fs = first.fs().clone();
    let second = Regd::load(fs, RecordingSink::default()).unwrap();
    assert_eq!(second.get("sys/a", ROOT).unwrap(), Some(&Value::I64(-1)));
    assert_eq!(
        second.get("user/1000/b", ALICE).unwrap(),
        Some(&push("kept"))
    );
}
