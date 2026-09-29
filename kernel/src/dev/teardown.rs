//! Releasing a device claim, on request or because its owner died (issue #240).
//!
//! One routine serves both, so a task that exits without calling `release`
//! leaves exactly what an orderly release leaves: the interrupt masked and the
//! claimant out of every delivery round, the function's decode and bus-master
//! enables cleared (a dead driver must not keep DMAing or decoding), its MMIO
//! unmapped and its quota returned, the device unowned with a bumped
//! generation so any old handle fails closed, and one audit record.
//!
//! There is no function-level reset yet (driver-plan risk 5): quiescing is the
//! command-register clear, which is what stops a device from touching memory.

use x86_64::PhysAddr;

use crate::mem::mmio;
use crate::quota::{self, Resource};

use super::claims::{Claim, CLAIMS};
use super::class::method;
use super::ops::release_va;
use super::report::{self, reason};
use super::syscall::quiesce;
use super::{table, DeviceId};

/// Release `id`'s claim on behalf of `actor`. `live_table` is the page table the
/// caller currently runs in (or the dying task's): mappings recorded against
/// another table are not walked, since that table may already be gone.
pub fn release_claim(id: DeviceId, actor: usize, why: u32, live_table: u64) {
    // Out of interrupt delivery first, so no message is posted for a claim
    // that is being torn down.
    let detached = CLAIMS.lock().detach(id);
    let Some(claim) = detached else { return };
    let info = table().lock().get(id);
    if let Some(info) = &info {
        quiesce(info);
    }
    let bytes = unmap_all(&claim, live_table);
    if bytes > 0 {
        quota::release(claim.uid, Resource::UserMemory, bytes);
    }
    quota::release(claim.uid, Resource::DeviceClaims, 1);
    let _ = table().lock().release_generation(id, claim.generation);
    if let Some(info) = &info {
        report::record(actor, info, method::RELEASE, true, why);
    }
}

/// Remove every BAR mapping of `claim`; returns the bytes to uncharge.
fn unmap_all(claim: &Claim, live_table: u64) -> u64 {
    let mut bytes = 0;
    for mapping in claim.maps.iter().flatten() {
        if mapping.table == live_table {
            mmio::unmap_mmio(
                PhysAddr::new(mapping.table),
                mapping.va,
                mapping.phys,
                mapping.pages,
            );
        }
        release_va(mapping.va, mapping.pages);
        bytes += mapping.pages * 4096;
    }
    bytes
}

/// Release every claim task `slot` holds. Called first by
/// [`crate::ipc::teardown_task`], before the task's handle table and address
/// space go away; `table` is the task's PML4.
pub fn teardown_task(slot: usize, table: u64) {
    let (ids, count) = CLAIMS.lock().owned_by(slot);
    for id in ids.iter().take(count).flatten() {
        release_claim(*id, slot, reason::TEARDOWN, table);
    }
}
