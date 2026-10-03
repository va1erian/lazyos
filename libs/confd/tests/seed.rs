//! Seeding `/conf` from the F0 to F3 store in `/data/confd`, exactly once
//! (`Confd::seed_once` and its marker).

mod common;

use common::{text, MemoryFs, ROOT};
use confd::dir::SEEDED_MARKER_FILE;
use confd::{persist, ChangeSink, Confd, Migration, ServiceError, Store, Value, STORE_FILE};

#[derive(Default)]
struct Sink;

impl ChangeSink for Sink {
    fn changed(&mut self, _path: &str, _deleted: bool) {}
}

/// A filesystem already holding `entries` as its committed store.
fn fs_with(entries: &[(&str, Value)]) -> MemoryFs {
    let mut store = Store::new();
    for (path, value) in entries {
        store.set(path, value.clone(), ROOT).unwrap();
    }
    let mut fs = MemoryFs::new();
    persist(&mut fs, &store).unwrap();
    fs
}

#[test]
fn seeds_once_then_never_again() {
    let mut svc = Confd::load(MemoryFs::new(), Sink).unwrap();
    let mut legacy = fs_with(&[
        ("sys/time/zone", text("Europe/Paris")),
        ("sys/a", Value::U64(1)),
    ]);
    let before = legacy.clone();

    let report = svc.seed_once(Some(&mut legacy)).unwrap();
    assert_eq!(
        report,
        Some(Migration {
            added: 2,
            skipped: 0
        })
    );
    assert!(svc.fs().file(SEEDED_MARKER_FILE).is_some());
    // The legacy store is read, never written or retired.
    assert_eq!(legacy.file(STORE_FILE), before.file(STORE_FILE));

    // A setting deleted after the migration does not come back.
    svc.delete("sys/a", ROOT).unwrap();
    assert_eq!(svc.seed_once(Some(&mut legacy)).unwrap(), None);
    assert_eq!(svc.get("sys/a", ROOT).unwrap(), None);

    // Nor after a restart on the same store.
    let mut restarted = Confd::load(svc.fs().clone(), Sink).unwrap();
    assert_eq!(restarted.seed_once(Some(&mut legacy)).unwrap(), None);
    assert_eq!(restarted.get("sys/a", ROOT).unwrap(), None);
    assert_eq!(
        restarted.get("sys/time/zone", ROOT).unwrap(),
        Some(&text("Europe/Paris"))
    );
}

#[test]
fn live_values_win_over_the_seed() {
    let mut svc = Confd::load(fs_with(&[("sys/time/zone", text("UTC"))]), Sink).unwrap();
    let mut legacy = fs_with(&[("sys/time/zone", text("Europe/Paris"))]);
    let report = svc.seed_once(Some(&mut legacy)).unwrap();
    assert_eq!(
        report,
        Some(Migration {
            added: 0,
            skipped: 0
        })
    );
    assert_eq!(svc.get("sys/time/zone", ROOT).unwrap(), Some(&text("UTC")));
}

#[test]
fn an_absent_legacy_store_still_marks_the_seed_done() {
    let mut svc = Confd::load(MemoryFs::new(), Sink).unwrap();
    assert_eq!(
        svc.seed_once::<MemoryFs>(None).unwrap(),
        Some(Migration::default())
    );
    assert!(svc.fs().file(SEEDED_MARKER_FILE).is_some());
    // A /data/confd that shows up later is not read.
    let mut late = fs_with(&[("sys/a", Value::Bool(true))]);
    assert_eq!(svc.seed_once(Some(&mut late)).unwrap(), None);
    assert!(svc.store().is_empty());
}

#[test]
fn a_corrupt_legacy_store_is_left_alone() {
    let mut svc = Confd::load(MemoryFs::new(), Sink).unwrap();
    let mut legacy = MemoryFs::new();
    legacy.put(STORE_FILE, b"not a store");
    assert_eq!(
        svc.seed_once(Some(&mut legacy)).unwrap(),
        Some(Migration::default())
    );
    assert_eq!(legacy.file(STORE_FILE), Some(&b"not a store"[..]));
}

#[test]
fn a_failed_seed_writes_no_marker_and_is_retried() {
    let mut legacy = fs_with(&[("sys/a", Value::U64(1))]);
    let mut live = MemoryFs::new();
    live.fail_writes();
    let mut svc = Confd::load(live, Sink).unwrap();
    assert_eq!(svc.seed_once(Some(&mut legacy)), Err(ServiceError::Io));
    assert!(svc.fs().file(SEEDED_MARKER_FILE).is_none());

    // An unreadable legacy store is a failure too, not an empty seed.
    let mut svc = Confd::load(MemoryFs::new(), Sink).unwrap();
    let mut unreadable = fs_with(&[("sys/a", Value::U64(1))]);
    unreadable.fail_reads();
    assert_eq!(svc.seed_once(Some(&mut unreadable)), Err(ServiceError::Io));
    assert!(svc.fs().file(SEEDED_MARKER_FILE).is_none());
    assert_eq!(
        svc.seed_once(Some(&mut legacy)).unwrap(),
        Some(Migration {
            added: 1,
            skipped: 0
        })
    );
}
