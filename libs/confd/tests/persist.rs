//! Crash-safe persist/load behaviour, including fault injection at every
//! step of a write.

mod common;

use common::{MemoryFs, ROOT};
use confd::{load, persist, Store, Value, CORRUPT_FILE, STORE_FILE, TMP_FILE};

fn old_store() -> Store {
    let mut store = Store::new();
    store.set("sys/keep", Value::U64(1), ROOT).unwrap();
    store.set("sys/old", Value::Bool(true), ROOT).unwrap();
    store
}

fn new_store() -> Store {
    let mut store = Store::new();
    store.set("sys/keep", Value::U64(2), ROOT).unwrap();
    store.set("sys/new", Value::Bool(false), ROOT).unwrap();
    store
}

#[test]
fn persist_writes_then_load_reads_back() {
    let mut fs = MemoryFs::new();
    let store = new_store();
    persist(&mut fs, &store).unwrap();

    assert_eq!(fs.file(TMP_FILE), None);
    assert!(fs.file(STORE_FILE).is_some());
    assert_eq!(load(&mut fs).unwrap(), store);
}

#[test]
fn loading_a_missing_store_is_empty() {
    let mut fs = MemoryFs::new();
    assert_eq!(load(&mut fs).unwrap(), Store::new());
    assert_eq!(fs.file(CORRUPT_FILE), None);
}

#[test]
fn load_discards_a_leftover_temporary_file() {
    let mut fs = MemoryFs::new();
    fs.put(TMP_FILE, b"half-written garbage");
    let mut store = Store::new();
    store.set("sys/a", Value::Bool(true), ROOT).unwrap();
    fs.put(STORE_FILE, &confd::encode(&store));

    // The complete committed store is what survives.
    assert_eq!(load(&mut fs).unwrap(), store);
    assert_eq!(fs.file(TMP_FILE), None);
}

#[test]
fn load_promotes_a_complete_temporary_file_left_without_a_store() {
    // A power cut inside a rename that dropped the destination entry first
    // (ext2): the fsynced new store survives only as the temporary file.
    let mut fs = MemoryFs::new();
    let store = new_store();
    fs.put(TMP_FILE, &confd::encode(&store));

    assert_eq!(load(&mut fs).unwrap(), store);
    assert_eq!(fs.file(TMP_FILE), None);
    assert_eq!(load(&mut fs).unwrap(), store, "the promotion is durable");
}

#[test]
fn load_drops_an_incomplete_temporary_file_left_without_a_store() {
    let mut fs = MemoryFs::new();
    let bytes = confd::encode(&new_store());
    fs.put(TMP_FILE, &bytes[..bytes.len() - 1]);

    assert_eq!(load(&mut fs).unwrap(), Store::new());
    assert_eq!(fs.file(TMP_FILE), None);
    assert_eq!(fs.file(STORE_FILE), None);
}

#[test]
fn load_moves_a_corrupt_store_aside() {
    let mut fs = MemoryFs::new();
    fs.put(STORE_FILE, b"CONFD not really an encoded store");

    assert_eq!(load(&mut fs).unwrap(), Store::new());
    assert_eq!(fs.file(STORE_FILE), None);
    assert_eq!(
        fs.file(CORRUPT_FILE),
        Some(b"CONFD not really an encoded store".as_slice())
    );
}

#[test]
fn load_replaces_an_older_corrupt_copy() {
    let mut fs = MemoryFs::new();
    fs.put(CORRUPT_FILE, b"older damage");
    fs.put(STORE_FILE, b"newer damage");

    assert_eq!(load(&mut fs).unwrap(), Store::new());
    assert_eq!(fs.file(CORRUPT_FILE), Some(b"newer damage".as_slice()));
}

#[test]
fn load_propagates_filesystem_errors() {
    // Clearing a leftover temporary file next to a store must not fail
    // silently.
    let mut fs = MemoryFs::new();
    fs.put(STORE_FILE, &confd::encode(&old_store()));
    fs.put(TMP_FILE, b"half-written garbage");
    fs.fail_removes();
    assert!(load(&mut fs).is_err());

    let mut fs = MemoryFs::new();
    fs.fail_reads();
    assert!(load(&mut fs).is_err());

    // A corrupt store that cannot be renamed aside is an error, not a silent
    // wipe of the committed file.
    let mut fs = MemoryFs::new();
    fs.put(STORE_FILE, b"junk");
    fs.fail_renames();
    assert!(load(&mut fs).is_err());
    assert_eq!(fs.file(STORE_FILE), Some(b"junk".as_slice()));

    // The leftover temporary file is still cleared before the error surfaces.
    let mut fs = MemoryFs::new();
    fs.put(TMP_FILE, b"half");
    fs.put(STORE_FILE, b"junk");
    fs.fail_renames();
    assert!(load(&mut fs).is_err());
    assert_eq!(fs.file(TMP_FILE), None);
    assert_eq!(fs.file(STORE_FILE), Some(b"junk".as_slice()));
}

#[test]
fn persist_propagates_filesystem_errors() {
    let store = new_store();
    for fail in [
        MemoryFs::fail_writes,
        MemoryFs::fail_fsyncs,
        MemoryFs::fail_renames,
    ] {
        let mut fs = MemoryFs::new();
        fail(&mut fs);
        assert!(persist(&mut fs, &store).is_err());
    }
    // A failed fsync must leave the committed store untouched.
    let old = old_store();
    let mut fs = MemoryFs::new();
    persist(&mut fs, &old).unwrap();
    fs.fail_fsyncs();
    assert!(persist(&mut fs, &new_store()).is_err());
    assert_eq!(load(&mut fs).unwrap(), old);
}

#[test]
fn crash_at_every_persist_step_leaves_old_or_new_store() {
    let old = old_store();
    let new = new_store();

    let mut base = MemoryFs::new();
    persist(&mut base, &old).unwrap();
    let steps = base.ops();
    assert_eq!(steps, 3, "persist is write, fsync, rename");

    // Fuel `n` allows exactly `n` operations, so each value stops persist at
    // a different step; the final value completes it. Partial writes leave a
    // torn temporary file behind, which is the worst case a writer can leave
    // without violating rename's atomicity.
    for fuel in 0..=steps + 2 {
        for partial in [false, true] {
            let mut fs = base.clone();
            fs.partial_writes(partial);
            fs.arm(fuel);
            let outcome = persist(&mut fs, &new);
            fs.disarm();

            let loaded = load(&mut fs).expect("load must recover");
            assert!(
                loaded == old || loaded == new,
                "fuel={fuel} partial={partial} yielded a torn store"
            );
            if fuel >= steps {
                assert!(outcome.is_ok(), "fuel={fuel} should complete");
                assert_eq!(loaded, new, "fuel={fuel} must load the new store");
            } else {
                assert!(outcome.is_err(), "fuel={fuel} should fail");
                assert_eq!(loaded, old, "fuel={fuel} must keep the old store");
            }
        }
    }
}

#[test]
fn crash_during_the_first_persist_leaves_empty_or_new() {
    let new = new_store();
    let mut completed = MemoryFs::new();
    persist(&mut completed, &new).unwrap();
    let steps = completed.ops();

    for fuel in 0..=steps {
        for partial in [false, true] {
            let mut fs = MemoryFs::new();
            fs.partial_writes(partial);
            fs.arm(fuel);
            let _ = persist(&mut fs, &new);
            fs.disarm();

            let loaded = load(&mut fs).unwrap();
            assert!(
                loaded == Store::new() || loaded == new,
                "fuel={fuel} partial={partial} yielded a torn store"
            );
        }
    }
}

#[test]
fn a_failed_persist_can_be_retried() {
    let old = old_store();
    let new = new_store();
    let mut fs = MemoryFs::new();
    persist(&mut fs, &old).unwrap();

    fs.arm(1);
    assert!(persist(&mut fs, &new).is_err());
    fs.disarm();
    let loaded = load(&mut fs).unwrap();
    assert!(loaded == old || loaded == new);

    fs.disarm();
    persist(&mut fs, &new).unwrap();
    assert_eq!(load(&mut fs).unwrap(), new);
}
