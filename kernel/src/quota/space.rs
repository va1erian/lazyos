//! Per-address-space user-memory charges, so teardown refunds the right uid.

use super::{charge, release, QuotaError, Resource};
use alloc::vec::Vec;
use spin::Mutex;

/// User-memory bytes each address space currently holds a charge for, keyed by
/// the space's PML4 and the uid that was charged.
///
/// The charge is per uid, but the *lifetime* is per address space: the bytes a
/// task `mmap`ed or `brk`ed are still charged when it exits without unmapping
/// them, so teardown must give them back ([`forget_address_space`]), and to
/// the uid that was actually charged even if the task's identity changed since.
/// `SPACES` is a leaf lock taken after `QUOTAS` is released, never nested in it.
struct SpaceCharge {
    table: u64,
    uid: u32,
    bytes: u64,
}

static SPACES: Mutex<Vec<SpaceCharge>> = Mutex::new(Vec::new());

/// The address space charges against `slot` are recorded under: `slot`'s own
/// PML4, not whatever table happens to be active on the CPU right now.
///
/// A syscall handler runs on its caller's table, so for the common case
/// (`slot == task::current()`) the two agree; but `charge_for_slot`/
/// `release_for_slot` take an explicit slot precisely so a caller (or a test)
/// can act on a *different* task, and the active CR3 would then key the charge
/// under the wrong address space -- `forget_address_space` could never release
/// it when that space is freed, and a release could wrongly drain whatever
/// space happened to be active. A slot with no task yet (a narrow window
/// during spawn) falls back to the active table, matching the old behaviour.
fn space_of(slot: usize) -> u64 {
    crate::task::pml4_of(slot).unwrap_or_else(|| crate::mem::kernel_table().as_u64())
}

/// Charge `delta` bytes of user memory to `slot`'s uid and record it against
/// `slot`'s own address space.
fn charge_user_memory(slot: usize, delta: u64) -> Result<(), QuotaError> {
    let uid = crate::ipc::credentials::of(slot).uid;
    charge_space(uid, space_of(slot), delta)
}

/// Charge `delta` bytes of user memory to `uid` and record it against the
/// address space whose PML4 is `table`, which need not belong to a task yet:
/// the loader charges a program's segments to the uid that will run it while
/// the new space is still being built (issue #265). Freeing the table
/// ([`forget_address_space`], called by `mem::free_user_table`) refunds it.
pub fn charge_space(uid: u32, table: u64, delta: u64) -> Result<(), QuotaError> {
    charge(uid, Resource::UserMemory, delta)?;
    let mut spaces = SPACES.lock();
    match spaces
        .iter_mut()
        .find(|space| space.table == table && space.uid == uid)
    {
        Some(space) => space.bytes = space.bytes.saturating_add(delta),
        None => spaces.push(SpaceCharge {
            table,
            uid,
            bytes: delta,
        }),
    }
    Ok(())
}

/// Release up to `delta` bytes of `slot`'s address space's user-memory
/// charge, preferring the caller's current uid. Bytes the space never charged
/// (its stacks, which the loader maps uncharged) release nothing: they must
/// not eat into another task's live usage. ELF segments are charged at load
/// (issue #265), so unmapping one gives its bytes back.
fn release_user_memory(slot: usize, delta: u64) {
    let current = crate::ipc::credentials::of(slot).uid;
    let table = space_of(slot);
    let mut remaining = delta;
    let mut releases: Vec<(u32, u64)> = Vec::new();
    {
        let mut spaces = SPACES.lock();
        for prefer_current in [true, false] {
            for space in spaces
                .iter_mut()
                .filter(|space| space.table == table && (space.uid == current) == prefer_current)
            {
                let take = space.bytes.min(remaining);
                if take > 0 {
                    space.bytes -= take;
                    remaining -= take;
                    releases.push((space.uid, take));
                }
            }
        }
        spaces.retain(|space| space.bytes > 0);
    }
    for (uid, bytes) in releases {
        release(uid, Resource::UserMemory, bytes);
    }
}

/// Give back every user-memory byte the address space `table` still holds a
/// charge for. Called when the space is freed (last user reaped), so a task
/// that exits without unmapping cannot strand its uid's quota.
pub fn forget_address_space(table: u64) {
    let mut freed: Vec<(u32, u64)> = Vec::new();
    {
        let mut spaces = SPACES.lock();
        spaces.retain(|space| {
            if space.table == table {
                freed.push((space.uid, space.bytes));
                false
            } else {
                true
            }
        });
    }
    for (uid, bytes) in freed {
        release(uid, Resource::UserMemory, bytes);
    }
}

/// Charge `resource` to the uid stamped on task `slot`.
///
/// The uid is read from [`crate::ipc::credentials`] under `CREDS`, then the
/// quota lock is taken (`CREDS` -> `QUOTAS`, the documented order).
/// [`Resource::UserMemory`] is additionally recorded per address space (see
/// [`SpaceCharge`]).
pub fn charge_for_slot(slot: usize, resource: Resource, delta: u64) -> Result<(), QuotaError> {
    if resource == Resource::UserMemory {
        return charge_user_memory(slot, delta);
    }
    let uid = crate::ipc::credentials::of(slot).uid;
    charge(uid, resource, delta)
}

/// Release `resource` from the uid stamped on task `slot`; see [`release`].
pub fn release_for_slot(slot: usize, resource: Resource, delta: u64) {
    if resource == Resource::UserMemory {
        return release_user_memory(slot, delta);
    }
    let uid = crate::ipc::credentials::of(slot).uid;
    release(uid, resource, delta);
}

/// Forget every recorded space charge (part of [`super::reset`]).
pub(super) fn reset() {
    SPACES.lock().clear();
}
