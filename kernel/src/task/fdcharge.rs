//! Per-uid descriptor charges (`Resource::Fds`, issue #483).
//!
//! Every open slot of an [`FdTable`](super::FdTable) holds one `Fds` charge,
//! recorded *in the slot* as the uid that paid, so a release always credits
//! the uid that was charged even if the task's identity changed since. The
//! acting task pays: `open`, `dup`, `dup2`, `fork` and the copies a spawn
//! makes charge the uid stamped on [`super::current`]. A uid at its limit gets
//! the same refusal as a full table (`EMFILE`).
//!
//! Copies that only mirror another table (`CLONE_FILES` peers,
//! [`super::fdshare`]) are uncharged: a share group's descriptors are paid for
//! once, by the table the operation ran in.
//!
//! Lock order: called under `TASKS`; takes `CREDS` then `QUOTAS`, both leaves.

use crate::quota::{self, Resource};

/// The uid the current operation charges.
pub(super) fn acting_uid() -> u32 {
    crate::ipc::credentials::of(super::current()).uid
}

/// Charge `count` descriptors to `uid`; false when its quota refuses.
pub(super) fn charge(uid: u32, count: u64) -> bool {
    count == 0 || quota::charge(uid, Resource::Fds, count).is_ok()
}

/// Give one descriptor back to `uid`.
pub(super) fn release(uid: u32) {
    quota::release(uid, Resource::Fds, 1);
}
