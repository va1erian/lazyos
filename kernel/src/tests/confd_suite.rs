//! Configuration registry service core (issue #260).
//!
//! The `confd` binary is ring-3, so the suite drives the same `confd::Confd`
//! commit logic the binary links, against an in-memory `StoreFs` and a
//! recording change sink. Every path through the service is exercised:
//! uid checks, list filtering, persist-before-swap, the change-topic filter
//! and reload across a restart.

use super::*;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use confd::{Caller, ChangeSink, Confd, ServiceError, StoreFs, Value};

/// Callers used throughout (mirrors the store crate's own test constants).
const ROOT: Caller = Caller { uid: 0 };
const ALICE: Caller = Caller { uid: 1000 };
const BOB: Caller = Caller { uid: 1001 };

/// An in-memory [`StoreFs`] with an injectable write failure.
#[derive(Clone, Default)]
struct MemFs {
    files: BTreeMap<String, Vec<u8>>,
    fail_writes: bool,
}

impl StoreFs for MemFs {
    type Error = ();

    fn read_file(&mut self, name: &str) -> Result<Option<Vec<u8>>, ()> {
        Ok(self.files.get(name).cloned())
    }

    fn write_file(&mut self, name: &str, data: &[u8]) -> Result<(), ()> {
        if self.fail_writes {
            return Err(());
        }
        self.files.insert(String::from(name), data.to_vec());
        Ok(())
    }

    fn fsync(&mut self, _name: &str) -> Result<(), ()> {
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), ()> {
        match self.files.remove(from) {
            Some(data) => {
                self.files.insert(String::from(to), data);
                Ok(())
            }
            None => Err(()),
        }
    }

    fn remove(&mut self, name: &str) -> Result<(), ()> {
        self.files.remove(name);
        Ok(())
    }
}

/// A sink that records every announcement.
#[derive(Clone, Default)]
struct RecordingSink {
    events: Vec<(String, bool)>,
}

impl ChangeSink for RecordingSink {
    fn changed(&mut self, path: &str, deleted: bool) {
        self.events.push((String::from(path), deleted));
    }
}

type Service = Confd<MemFs, RecordingSink>;

fn service() -> Result<Service, String> {
    Confd::load(MemFs::default(), RecordingSink::default()).map_err(fail)
}

fn text(value: &str) -> Value {
    Value::Str(String::from(value))
}

fn events(service: &Service) -> &[(String, bool)] {
    &service.sink().events
}

/// set/get/list/delete, including the absent-delete no-op.
pub fn set_get_list_delete() -> Result<(), String> {
    let mut confd = service()?;
    check!(
        confd.get("sys/net/mtu", ROOT).map_err(fail)? == None,
        "a fresh store is not empty"
    );

    confd
        .set("sys/net/mtu", Value::U64(1500), ROOT)
        .map_err(fail)?;
    confd
        .set("sys/net/name", text("eth0"), ROOT)
        .map_err(fail)?;
    let value = confd.get("sys/net/mtu", ROOT).map_err(fail)?;
    check!(
        value == Some(&Value::U64(1500)),
        "get returned the wrong value"
    );
    check!(
        confd.list("sys/net", ROOT).map_err(fail)? == ["sys/net/mtu", "sys/net/name"],
        "list returned the wrong paths"
    );

    confd.delete("sys/net/mtu", ROOT).map_err(fail)?;
    check!(
        confd.get("sys/net/mtu", ROOT).map_err(fail)? == None,
        "delete left the value"
    );
    // Deleting an absent path is a no-op, not an error.
    confd.delete("sys/net/mtu", ROOT).map_err(fail)?;
    Ok(())
}

/// Any caller may read `sys/`, but only uid 0 may write it.
pub fn non_root_denied_on_sys() -> Result<(), String> {
    let mut confd = service()?;
    check!(
        confd.set("sys/net/mtu", Value::U64(1), ALICE) == Err(ServiceError::Denied),
        "a non-root caller wrote sys/"
    );
    check!(
        confd
            .set("sys/net/mtu", Value::U64(2), ROOT)
            .map_err(fail)?
            == (),
        "root could not write sys/"
    );
    // sys/ is world-readable.
    check!(
        confd.get("sys/net/mtu", BOB).map_err(fail)? == Some(&Value::U64(2)),
        "a non-root caller could not read sys/"
    );
    Ok(())
}

/// `user/<uid>` is owner-or-root only, for reads and writes alike.
pub fn cross_user_denied() -> Result<(), String> {
    let mut confd = service()?;
    confd
        .set("user/1000/theme", text("dark"), ALICE)
        .map_err(fail)?;
    check!(
        confd.get("user/1000/theme", ALICE).map_err(fail)? == Some(&text("dark")),
        "the owner could not read its own path"
    );
    check!(
        confd.get("user/1000/theme", BOB) == Err(ServiceError::Denied),
        "a stranger read another user's path"
    );
    check!(
        confd.set("user/1000/theme", text("light"), BOB) == Err(ServiceError::Denied),
        "a stranger wrote another user's path"
    );
    // Root may read and write any user subtree.
    check!(
        confd.get("user/1000/theme", ROOT).map_err(fail)? == Some(&text("dark")),
        "root could not read a user path"
    );
    check!(
        confd
            .set("user/1000/theme", text("light"), ROOT)
            .map_err(fail)?
            == (),
        "root could not write a user path"
    );
    Ok(())
}

/// List filters out paths the caller may not read.
pub fn list_filters_other_users() -> Result<(), String> {
    let mut confd = service()?;
    confd
        .set("user/1000/a", Value::Bool(true), ALICE)
        .map_err(fail)?;
    confd
        .set("user/1001/b", Value::Bool(true), BOB)
        .map_err(fail)?;
    confd.set("sys/a", Value::Bool(true), ROOT).map_err(fail)?;
    check!(
        confd.list("", ALICE).map_err(fail)? == ["sys/a", "user/1000/a"],
        "a caller saw another user's paths"
    );
    check!(
        confd.list("user", ALICE).map_err(fail)? == ["user/1000/a"],
        "a user list leaked another subtree"
    );
    Ok(())
}

/// A committed `sys/` change is announced with only `(path, deleted)`.
pub fn change_topic_delivery() -> Result<(), String> {
    let mut confd = service()?;
    confd
        .set("sys/net/mtu", Value::U64(1500), ROOT)
        .map_err(fail)?;
    confd.delete("sys/net/mtu", ROOT).map_err(fail)?;
    check!(
        events(&confd)
            == [
                (String::from("sys/net/mtu"), false),
                (String::from("sys/net/mtu"), true),
            ],
        "change topics were {:?}",
        events(&confd)
    );
    Ok(())
}

/// A `user/` change commits but is deliberately not announced (the kernel
/// topic policy cannot enforce per-uid subscriptions).
pub fn user_changes_are_silent() -> Result<(), String> {
    let mut confd = service()?;
    confd
        .set("user/1000/theme", text("dark"), ALICE)
        .map_err(fail)?;
    confd.delete("user/1000/theme", ALICE).map_err(fail)?;
    check!(
        confd.get("user/1000/theme", ALICE).map_err(fail)? == None,
        "the user change did not commit"
    );
    check!(
        events(&confd).is_empty(),
        "a user/ change was announced: {:?}",
        events(&confd)
    );
    Ok(())
}

/// A persist failure leaves the committed store and the published state
/// untouched.
pub fn io_leaves_store_unchanged() -> Result<(), String> {
    let mut seeded = service()?;
    seeded.set("sys/keep", Value::U64(1), ROOT).map_err(fail)?;
    let mut broken = seeded.fs().clone();
    broken.fail_writes = true;
    let mut confd = Confd::load(broken, RecordingSink::default()).map_err(fail)?;

    check!(
        confd.set("sys/new", Value::U64(2), ROOT) == Err(ServiceError::Io),
        "a failing write did not surface CONFD_IO"
    );
    check!(
        confd.get("sys/new", ROOT).map_err(fail)? == None,
        "a failed write changed the store"
    );
    check!(
        confd.get("sys/keep", ROOT).map_err(fail)? == Some(&Value::U64(1)),
        "a failed write disturbed an existing key"
    );
    check!(
        events(&confd).is_empty(),
        "a failed write announced a change"
    );
    Ok(())
}

/// A restart reloads the store from the same backing filesystem.
pub fn restart_reloads_store() -> Result<(), String> {
    let mut first = service()?;
    first.set("sys/a", Value::I64(-1), ROOT).map_err(fail)?;
    first
        .set("user/1000/b", text("kept"), ALICE)
        .map_err(fail)?;

    let fs = first.fs().clone();
    let second = Confd::load(fs, RecordingSink::default()).map_err(fail)?;
    check!(
        second.get("sys/a", ROOT).map_err(fail)? == Some(&Value::I64(-1)),
        "the system value did not reload"
    );
    check!(
        second.get("user/1000/b", ALICE).map_err(fail)? == Some(&text("kept")),
        "the user value did not reload"
    );
    Ok(())
}

/// Many writes from several owners, with restarts reloading the same data.
pub fn soak_sets_and_restarts() -> Result<(), String> {
    const WRITES: usize = 4000;
    const RESTART_EVERY: usize = 500;
    let owners = [ROOT, ALICE, BOB];
    let mut confd = service()?;
    let mut written = 0usize;

    for index in 0..WRITES {
        let owner = owners[index % owners.len()];
        let scope = if owner.uid == 0 {
            "sys/soak"
        } else {
            // Only the owner may write its own subtree, so the path must match
            // the caller or every write would be denied.
            match owner.uid {
                1000 => "user/1000/soak",
                _ => "user/1001/soak",
            }
        };
        let path = format!("{scope}/{index}");
        confd
            .set(&path, Value::U64(index as u64), owner)
            .map_err(fail)?;
        written += 1;

        if index % RESTART_EVERY == RESTART_EVERY - 1 {
            // Restart: reload from the same filesystem and confirm the writes
            // survived.
            let fs = confd.fs().clone();
            confd = Confd::load(fs, RecordingSink::default()).map_err(fail)?;
            let probe = format!("{scope}/{index}");
            check!(
                confd.get(&probe, owner).map_err(fail)? == Some(&Value::U64(index as u64)),
                "write {index} did not survive a restart"
            );
        }
    }

    check!(
        confd.store().len() == written,
        "the store holds {} entries, expected {written}",
        confd.store().len()
    );
    Ok(())
}

/// `ServiceError` has no `String` conversion in `no_std`; the tests only need
/// a printable detail for `check!`.
fn fail(error: ServiceError) -> String {
    String::from(error.message())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("confd_set_get_list_delete", set_get_list_delete),
    ("confd_non_root_denied_on_sys", non_root_denied_on_sys),
    ("confd_cross_user_denied", cross_user_denied),
    ("confd_list_filters_other_users", list_filters_other_users),
    ("confd_change_topic_delivery", change_topic_delivery),
    ("confd_user_changes_are_silent", user_changes_are_silent),
    ("confd_io_leaves_store_unchanged", io_leaves_store_unchanged),
    ("confd_restart_reloads_store", restart_reloads_store),
    ("confd_soak_sets_and_restarts", soak_sets_and_restarts),
];
