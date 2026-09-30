//! Releasing a device claim, on request or because its owner died (issue #240).
//!
//! One routine serves both, so a task that exits without calling `release`
//! leaves exactly what an orderly release leaves: the interrupt masked and the
//! claimant out of every delivery round, the function's decode and bus-master
//! enables cleared (a dead driver must not keep DMAing or decoding), its MMIO
//! unmapped and its quota returned, the device unowned with a bumped
//! generation so any old handle fails closed, and one audit record.
//!
//! A task that has *died* but not been reaped is a zombie: its address space
//! (and so its MMIO mappings) lives on until the parent reaps it, so the claim
//! itself is released then. The dangerous part, a live interrupt line and a
//! device that can still DMA, is stopped at exit by [`silence_exited`].
//!
//! There is no function-level reset yet (driver-plan risk 5): quiescing is the
//! command-register clear, which is what stops a device from touching memory.

use core::sync::atomic::{AtomicU64, Ordering};

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
    // Bus mastering is off, so the device can no longer write these frames.
    // Close the owner's reference to each DMA buffer *by object id* (a
    // transferred handle may have been reused): this drops the owner's mapping
    // and, when no client or message still holds the buffer, frees the run and
    // releases its `DmaMemory` charge. A buffer a client still holds keeps its
    // frames and charge until that last reference goes.
    for record in claim.dma.iter().flatten() {
        crate::ipc::shared::close_owned_for_task(claim.owner, record.object_id, true);
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
    // The slot is about to be reused: a stale exit mark must not silence its
    // next owner.
    if slot < 64 {
        EXITED.fetch_and(!(1 << slot), Ordering::AcqRel);
    }
    let (ids, count) = CLAIMS.lock().owned_by(slot);
    for id in ids.iter().take(count).flatten() {
        release_claim(*id, slot, reason::TEARDOWN, table);
    }
}

const _: () = assert!(crate::task::MAX_TASKS <= 64, "EXITED is one u64");

/// Bit per task slot that died since the last [`silence_exited`].
static EXITED: AtomicU64 = AtomicU64::new(0);

/// Record that task `slot` has just died. Lock-free, so the scheduler can call
/// it from the timer sweep with its own locks held; [`silence_exited`] does the
/// work later, from task context.
pub fn note_task_exited(slot: usize) {
    if slot < 64 {
        EXITED.fetch_or(1 << slot, Ordering::AcqRel);
    }
}

/// Whether a dead task's claims still wait to be silenced.
pub fn exits_pending() -> bool {
    EXITED.load(Ordering::Acquire) != 0
}

/// Stop every device claimed by a task that died since the last call (issue
/// #283): take the claim out of interrupt delivery, mask a line nobody else
/// listens on, and clear the function's decode and bus-master enables. The
/// claim, its mappings and its quota stay until the zombie is reaped, when
/// [`teardown_task`] frees them. Idempotent.
pub fn silence_exited() {
    // The mux calls this with interrupts on and is preemptible: hold neither
    // the claim lock nor the PCI address port across a switch.
    x86_64::instructions::interrupts::without_interrupts(silence_exited_locked);
}

fn silence_exited_locked() {
    let mut dead = EXITED.swap(0, Ordering::AcqRel);
    while dead != 0 {
        let slot = dead.trailing_zeros() as usize;
        dead &= dead - 1;
        let (ids, count) = CLAIMS.lock().owned_by(slot);
        for id in ids.iter().take(count).flatten() {
            // Stop the device first so a shared line is not unmasked while the
            // dead function still asserts it.
            let info = table().lock().get(*id);
            if let Some(info) = &info {
                quiesce(info);
            }
            CLAIMS.lock().silence(*id);
        }
    }
}
