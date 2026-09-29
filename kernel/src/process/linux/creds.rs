//! Linux credential syscalls (`getuid` family, `setuid` family), backed by the
//! kernel's per-task [`Cred`] (issue #231).
//!
//! LazyOS keeps one uid and one gid per task, so the real/effective/saved
//! distinction collapses: `getuid`/`geteuid` agree, and the `setre*id` and
//! `setres*id` calls are accepted only when every id they name is `-1`
//! (unchanged), the caller's current id, or one common new value. Anything that
//! would need split ids is refused with `EPERM` rather than silently
//! approximated, and every real change goes through the audited, never-widening
//! [`credentials::transition`] gate, so an unprivileged caller can neither
//! escalate nor be told a refused change succeeded.

use crate::ipc::credentials::{self, Cred};

use super::errno::{err, EINVAL, EPERM};

/// `-1` in a `uid_t`/`gid_t` argument: "leave this id unchanged".
const UNCHANGED: u32 = u32::MAX;

/// The calling task's credentials.
fn current() -> Cred {
    credentials::of(crate::task::current())
}

/// `(uid, gid)` of the calling task, for the `execve` auxiliary vector.
pub(super) fn ids() -> (u32, u32) {
    let cred = current();
    (cred.uid, cred.gid)
}

pub(super) fn sys_getuid() -> u64 {
    u64::from(current().uid)
}

pub(super) fn sys_getgid() -> u64 {
    u64::from(current().gid)
}

/// Apply a uid and/or gid change to the caller; `0` or `-EPERM`.
///
/// Leaving uid 0 sheds every capability, as Linux does when all uids become
/// non-zero: a dropped process must not keep the power to undo the drop.
fn apply(uid: Option<u32>, gid: Option<u32>) -> u64 {
    let slot = crate::task::current();
    let now = credentials::of(slot);
    let mut requested = now;
    if let Some(uid) = uid {
        requested.uid = uid;
        if uid != 0 {
            requested.caps = 0;
        }
    }
    if let Some(gid) = gid {
        requested.gid = gid;
    }
    if requested == now {
        return 0; // Setting an id to its current value needs no privilege.
    }
    match credentials::transition(slot, slot, requested) {
        Ok(_) => 0,
        Err(_) => err(EPERM),
    }
}

/// Apply a single-id change: `(uid_t)-1` is `EINVAL`, as on Linux.
fn set_one(value: u64, group: bool) -> u64 {
    let value = value as u32;
    if value == UNCHANGED {
        return err(EINVAL);
    }
    if group {
        apply(None, Some(value))
    } else {
        apply(Some(value), None)
    }
}

pub(super) fn sys_setuid(uid: u64) -> u64 {
    set_one(uid, false)
}

pub(super) fn sys_setgid(gid: u64) -> u64 {
    set_one(gid, true)
}

/// `setre[ug]id`/`setres[ug]id`: every named id must agree (see the module
/// docs). `group` selects the gid variant.
pub(super) fn sys_setres(a: u64, b: u64, c: u64, group: bool) -> u64 {
    let have = if group { current().gid } else { current().uid };
    let mut target: Option<u32> = None;
    for raw in [a, b, c] {
        let id = raw as u32;
        if id == UNCHANGED {
            continue;
        }
        match target {
            Some(seen) if seen != id => return err(EPERM),
            _ => target = Some(id),
        }
    }
    match target {
        None => 0,
        Some(id) if id == have => 0,
        Some(id) if group => apply(None, Some(id)),
        Some(id) => apply(Some(id), None),
    }
}
