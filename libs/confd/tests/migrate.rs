//! Moving settings into a better store (`/data/confd`) that became usable
//! after the service started, or that an earlier run could not reach.

mod common;

use common::{text, MemoryFs, SplitMix64, ROOT};
use confd::{
    persist, ChangeSink, Confd, Migration, ServiceError, Store, Value, MIGRATED_FILE, STORE_FILE,
};

#[derive(Default)]
struct Sink {
    events: Vec<(String, bool)>,
}

impl ChangeSink for Sink {
    fn changed(&mut self, path: &str, deleted: bool) {
        self.events.push((path.to_string(), deleted));
    }
}

type Svc = Confd<MemoryFs, Sink>;

fn service() -> Svc {
    Confd::load(MemoryFs::new(), Sink::default()).unwrap()
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

fn stored(fs: &MemoryFs) -> Store {
    confd::load(&mut fs.clone()).unwrap()
}

#[test]
fn rebind_seeds_an_empty_destination_and_keeps_serving() {
    let mut svc = service();
    svc.set("sys/time/zone", text("UTC"), ROOT).unwrap();
    svc.set("user/1000/theme", text("dark"), ROOT).unwrap();

    let report = svc.rebind(MemoryFs::new()).unwrap();
    assert_eq!(
        report,
        Migration {
            added: 2,
            skipped: 0
        }
    );

    // The new backing store holds the settings durably.
    assert_eq!(stored(svc.fs()), *svc.store());
    assert_eq!(svc.get("sys/time/zone", ROOT).unwrap(), Some(&text("UTC")));
    // Later writes land in the new store.
    svc.set("sys/a", Value::Bool(true), ROOT).unwrap();
    assert_eq!(stored(svc.fs()).len(), 3);
}

#[test]
fn rebind_never_clobbers_existing_destination_values() {
    let mut svc = service();
    svc.set("sys/time/zone", text("UTC"), ROOT).unwrap();
    svc.set("sys/only_live", Value::U64(7), ROOT).unwrap();
    let dest = fs_with(&[
        ("sys/time/zone", text("Europe/Paris")),
        ("sys/only_data", Value::U64(9)),
    ]);

    let report = svc.rebind(dest).unwrap();
    assert_eq!(
        report,
        Migration {
            added: 1,
            skipped: 0
        }
    );
    // /data wins the conflict, both unique values survive.
    assert_eq!(
        svc.get("sys/time/zone", ROOT).unwrap(),
        Some(&text("Europe/Paris"))
    );
    assert_eq!(
        svc.get("sys/only_live", ROOT).unwrap(),
        Some(&Value::U64(7))
    );
    assert_eq!(
        svc.get("sys/only_data", ROOT).unwrap(),
        Some(&Value::U64(9))
    );
    assert_eq!(stored(svc.fs()), *svc.store());
}

#[test]
fn rebind_announces_paths_whose_value_changed_for_readers() {
    let mut svc = service();
    svc.set("sys/conflict", Value::U64(1), ROOT).unwrap();
    svc.set("sys/same", Value::U64(2), ROOT).unwrap();
    svc.set("user/1000/x", Value::U64(3), ROOT).unwrap();
    let dest = fs_with(&[
        ("sys/conflict", Value::U64(10)),
        ("sys/same", Value::U64(2)),
        ("sys/fresh", Value::U64(4)),
    ]);
    svc.sink_mut().events.clear();
    svc.rebind(dest).unwrap();
    let mut seen = svc.sink().events.clone();
    seen.sort();
    // Only sys/ is announceable; `same` did not change, `user/` stays quiet.
    assert_eq!(
        seen,
        vec![
            ("sys/conflict".to_string(), false),
            ("sys/fresh".to_string(), false)
        ]
    );
}

#[test]
fn rebind_retires_the_old_store_so_deletions_are_not_resurrected() {
    let mut svc = service();
    svc.set("sys/a", Value::U64(1), ROOT).unwrap();
    svc.rebind(MemoryFs::new()).unwrap();
    svc.delete("sys/a", ROOT).unwrap();
    assert!(svc.get("sys/a", ROOT).unwrap().is_none());
    assert!(stored(svc.fs()).is_empty());
}

#[test]
fn rebind_failure_leaves_service_and_old_store_untouched() {
    let mut svc = service();
    svc.set("sys/a", Value::U64(1), ROOT).unwrap();
    let old_fs = svc.fs().clone();
    let before = svc.store().clone();

    for step in 0..4 {
        let mut dest = MemoryFs::new();
        dest.arm(step);
        assert_eq!(svc.rebind(dest), Err(ServiceError::Io), "fuel {step}");
        assert_eq!(*svc.store(), before);
        assert_eq!(svc.fs().file(STORE_FILE), old_fs.file(STORE_FILE));
        assert_eq!(svc.fs().file(MIGRATED_FILE), None);
    }
    let mut unreadable = MemoryFs::new();
    unreadable.fail_reads();
    assert_eq!(svc.rebind(unreadable), Err(ServiceError::Io));
    // Still fully usable, and a retry with a healthy destination succeeds.
    svc.set("sys/b", Value::U64(2), ROOT).unwrap();
    assert_eq!(svc.rebind(MemoryFs::new()).unwrap().added, 2);
}

#[test]
fn rebind_moves_a_corrupt_destination_aside_and_seeds_it() {
    let mut svc = service();
    svc.set("sys/a", Value::U64(1), ROOT).unwrap();
    let mut dest = MemoryFs::new();
    dest.put(STORE_FILE, b"garbage, not a store");
    svc.rebind(dest).unwrap();
    assert!(svc.fs().file(confd::CORRUPT_FILE).is_some());
    assert_eq!(stored(svc.fs()), *svc.store());
    assert_eq!(svc.get("sys/a", ROOT).unwrap(), Some(&Value::U64(1)));
}

#[test]
fn absorb_seeds_missing_values_then_retires_the_source() {
    let mut svc = service();
    svc.set("sys/keep", Value::U64(1), ROOT).unwrap();
    let mut source = fs_with(&[("sys/keep", Value::U64(99)), ("sys/extra", text("x"))]);

    let report = svc.absorb(&mut source).unwrap();
    assert_eq!(
        report,
        Migration {
            added: 1,
            skipped: 0
        }
    );
    assert_eq!(svc.get("sys/keep", ROOT).unwrap(), Some(&Value::U64(1)));
    assert_eq!(svc.get("sys/extra", ROOT).unwrap(), Some(&text("x")));
    assert_eq!(stored(svc.fs()), *svc.store());
    assert_eq!(source.file(STORE_FILE), None);
    assert!(source.file(MIGRATED_FILE).is_some());

    // A second absorb finds nothing to do, and a value deleted since is not
    // resurrected from the retired copy.
    svc.delete("sys/extra", ROOT).unwrap();
    assert_eq!(svc.absorb(&mut source).unwrap(), Migration::default());
    assert!(svc.get("sys/extra", ROOT).unwrap().is_none());
}

#[test]
fn absorb_of_a_missing_or_corrupt_source_is_harmless() {
    let mut svc = service();
    svc.set("sys/a", Value::U64(1), ROOT).unwrap();
    assert_eq!(
        svc.absorb(&mut MemoryFs::new()).unwrap(),
        Migration::default()
    );

    let mut corrupt = MemoryFs::new();
    corrupt.put(STORE_FILE, b"nope");
    assert_eq!(svc.absorb(&mut corrupt).unwrap(), Migration::default());
    assert_eq!(svc.store().len(), 1);
}

#[test]
fn absorb_persist_failure_keeps_live_store_and_source() {
    let mut live = fs_with(&[("sys/a", Value::U64(1))]);
    live.fail_writes();
    let mut svc = Confd::load(live, Sink::default()).unwrap();
    let before = svc.store().clone();
    let mut source = fs_with(&[("sys/b", Value::U64(2))]);
    assert_eq!(svc.absorb(&mut source), Err(ServiceError::Io));
    assert_eq!(*svc.store(), before);
    assert!(source.file(STORE_FILE).is_some());
}

#[test]
fn absorb_unreadable_source_is_an_io_error() {
    let mut svc = service();
    let mut source = MemoryFs::new();
    source.fail_reads();
    assert_eq!(svc.absorb(&mut source), Err(ServiceError::Io));
}

#[test]
fn oversized_entries_are_skipped_and_the_source_is_kept() {
    // Fill the live store to the limit, then absorb a source with one more
    // entry: it cannot fit, so it is skipped and the source file stays.
    let mut svc = service();
    let mut i = 0;
    while svc
        .set(
            &format!("sys/fill/{i}"),
            Value::Bytes(vec![0; confd::MAX_VALUE_LEN]),
            ROOT,
        )
        .is_ok()
    {
        i += 1;
    }
    let mut source = fs_with(&[("sys/too_big", Value::Bytes(vec![1; confd::MAX_VALUE_LEN]))]);
    let report = svc.absorb(&mut source).unwrap();
    assert_eq!(
        report,
        Migration {
            added: 0,
            skipped: 1
        }
    );
    assert!(source.file(STORE_FILE).is_some());
    assert!(svc.get("sys/too_big", ROOT).unwrap().is_none());
}

#[test]
fn soak_repeated_late_mounts_never_lose_or_clobber() {
    let mut rng = SplitMix64::new(0xC0FF_EE);
    // Model: path -> value, the live truth.
    let mut model = std::collections::BTreeMap::<String, u64>::new();
    let mut svc = service();
    for round in 0..300 {
        for _ in 0..20 {
            let path = format!("sys/k/{}", rng.below(40));
            if rng.below(4) == 0 {
                svc.delete(&path, ROOT).unwrap();
                model.remove(&path);
            } else {
                let v = rng.next_u64();
                svc.set(&path, Value::U64(v), ROOT).unwrap();
                model.insert(path, v);
            }
        }
        // A "data disk" appears with some pre-existing values that win.
        let mut dest_store = Store::new();
        let mut overrides = Vec::new();
        for _ in 0..rng.below(6) {
            let path = format!("sys/k/{}", rng.below(40));
            let v = rng.next_u64();
            dest_store.set(&path, Value::U64(v), ROOT).unwrap();
            overrides.push((path, v));
        }
        let mut dest = MemoryFs::new();
        persist(&mut dest, &dest_store).unwrap();
        if round % 7 == 0 {
            // A failed attempt first must change nothing.
            let mut bad = dest.clone();
            bad.fail_writes();
            let snapshot = svc.store().clone();
            if svc.rebind(bad).is_err() {
                assert_eq!(*svc.store(), snapshot);
            } else {
                // Nothing needed writing (the destination already held it all).
                panic!("rebind with failing writes must fail when it has data to write");
            }
        }
        svc.rebind(dest).unwrap();
        for (path, v) in overrides {
            model.insert(path, v);
        }
        // Live service agrees with the model, in memory and on disk.
        for (path, v) in &model {
            assert_eq!(
                svc.get(path, ROOT).unwrap(),
                Some(&Value::U64(*v)),
                "{path}"
            );
        }
        assert_eq!(svc.store().len(), model.len());
        assert_eq!(stored(svc.fs()), *svc.store());
    }
}
