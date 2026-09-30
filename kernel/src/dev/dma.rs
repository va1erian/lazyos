//! `dma_alloc`: physically contiguous frames as a shared `Buffer` handle plus
//! the bus address to program into the device (issue #241, driver-plan 3.4).
//!
//! A driver holding `DEV_DMA` asks for `len` bytes. The kernel takes a
//! contiguous run from the boot-time DMA pool (`mem::dma`), wraps it in the
//! ordinary shared-buffer object ([`shared::create_from_frames`]) so it can be
//! passed to a client zero-copy, and copies the run's physical address — the bus
//! address, until an IOMMU exists — back to the caller. The bytes are metered
//! by [`Resource::DmaMemory`] against the claimant's uid.
//!
//! Without an IOMMU a driver with `DEV_DMA` is trusted like the kernel
//! (driver-plan D5): it can program its device to write any physical address.
//! The ACL, the driver's dedicated uid and audit limit who can be that driver;
//! teardown clears bus mastering before any frame can be reused.
//!
//! Failure atomicity: every step undoes all earlier ones. The pool run is freed
//! and the `DmaMemory` charge released on any later failure; the claim's DMA
//! record is dropped first, then the buffer (which frees the run and the
//! charge), so nothing is released twice.

use alloc::vec::Vec;

use x86_64::PhysAddr;

use crate::ipc::handles;
use crate::ipc::shared::{self, DmaOwner};
use crate::quota::{self, Resource};
use crate::user_ptr;

use super::claims::{DmaRecord, CLAIMS};
use super::class::method;
use super::errno::*;
use super::report::{self, reason};
use super::syscall::Resolved;

/// Largest single DMA allocation. Bounds the pool a single call can hold and
/// the copy-out a client must map.
pub const MAX_DMA_BYTES: u64 = 4 << 20;
/// Physical address just past the 32-bit boundary.
const FOUR_GIB: u64 = 1 << 32;
const PAGE: u64 = 4096;

/// `dma_alloc` flag bits.
pub mod flag {
    /// Map the buffer only into the creator, never a client (`SHARE_ONLY`).
    pub const SHARE_ONLY: u64 = 1 << 0;
    /// The caller can address the whole 64-bit bus, so the run need not be
    /// below 4 GiB. The pool is below 4 GiB today, so this is currently a
    /// no-op kept for the IOMMU stage.
    pub const ADDR64: u64 = 1 << 1;
}

/// Record a failed `dma_alloc` and return `errno`.
fn refuse(r: &Resolved, code: u32, errno: Errno) -> Errno {
    report::record(r.claim.owner, &r.info, method::DMA, false, code);
    errno
}

/// Map a shared-buffer creation failure onto the device errno vocabulary.
fn map_error(error: shared::Error) -> Errno {
    match error {
        shared::Error::BadFlags | shared::Error::ExecutableDenied | shared::Error::BadSize => {
            EINVAL
        }
        shared::Error::MapFailed | shared::Error::OutOfMemory => ENOMEM,
        _ => EMFILE,
    }
}

/// `dma_alloc(handle, len, flags, out)`: allocate a contiguous run, hand it
/// back as a `Buffer` handle, and write its bus address to `*out`.
pub fn dma_alloc(r: &Resolved, len: u64, flags: u64, out: u64) -> Result<u64, Errno> {
    let slot = r.claim.owner;
    let uid = r.claim.uid;

    // Pure validation first: no side effects to undo.
    if flags & !(flag::SHARE_ONLY | flag::ADDR64) != 0 || len == 0 {
        return Err(refuse(r, reason::DMA_BAD_REQUEST, EINVAL));
    }
    let bytes = len
        .checked_add(PAGE - 1)
        .map(|value| value & !(PAGE - 1))
        .filter(|bytes| *bytes > 0 && *bytes <= MAX_DMA_BYTES)
        .ok_or_else(|| refuse(r, reason::DMA_BAD_REQUEST, EINVAL))?;
    let pages = bytes / PAGE;
    // Validate the destination now, so a hostile pointer costs no pool/quota
    // churn; the actual write happens only once everything else succeeded.
    if user_ptr::try_bytes(out, 8).is_err() {
        return Err(refuse(r, reason::DMA_FAULT, EFAULT));
    }

    // Buffers the driver closed or handed off since the last call no longer
    // need a record slot; without this a long-lived driver would run out of
    // its 16 slots after 16 allocations.
    prune_records(r);

    // Charge before allocating; every later failure releases it.
    if quota::charge(uid, Resource::DmaMemory, bytes).is_err() {
        return Err(refuse(r, reason::DMA_QUOTA, EDQUOT));
    }
    let Some(phys) = crate::mem::dma_alloc(pages, 1) else {
        quota::release(uid, Resource::DmaMemory, bytes);
        return Err(refuse(r, reason::DMA_NO_MEMORY, ENOMEM));
    };
    if flags & flag::ADDR64 == 0 && phys.as_u64().saturating_add(bytes) > FOUR_GIB {
        // The pool is below 4 GiB, so this is defensive only.
        free_run(phys, pages);
        quota::release(uid, Resource::DmaMemory, bytes);
        return Err(refuse(r, reason::DMA_BAD_REQUEST, EINVAL));
    }

    let frames: Vec<PhysAddr> = (0..pages)
        .map(|page| PhysAddr::new(phys.as_u64() + page * PAGE))
        .collect();
    let mut shared_flags = shared::flags::READ | shared::flags::WRITE | shared::flags::PINNED;
    if flags & flag::SHARE_ONLY != 0 {
        shared_flags |= shared::flags::SHARE_ONLY;
    }
    let owner = DmaOwner {
        device: r.id.0,
        generation: r.claim.generation,
        uid,
    };
    let handle = match shared::create_from_frames(frames, shared_flags, uid, owner) {
        Ok(handle) => handle,
        Err(error) => {
            // `create_from_frames` already returned the frames to the pool.
            quota::release(uid, Resource::DmaMemory, bytes);
            return Err(refuse(r, reason::DMA_BAD_REQUEST, map_error(error)));
        }
    };
    let object_id = match handles::get(handle) {
        Ok(entry) => entry.object_id,
        Err(_) => {
            let _ = shared::close_unseen(handle);
            return Err(refuse(r, reason::DMA_RECORD_FULL, EMFILE));
        }
    };
    let recorded = {
        let mut claims = CLAIMS.lock();
        match claims.get_mut(r.id) {
            Some(claim) if claim.generation == r.claim.generation => claim.record_dma(DmaRecord {
                object_id,
                pages,
                base: phys.as_u64(),
                quarantined: false,
            }),
            _ => false,
        }
    };
    if !recorded {
        // Closing the buffer frees the run and releases the charge. The device
        // never saw its address, so nothing needs quiescing.
        let _ = shared::close_unseen(handle);
        return Err(refuse(r, reason::DMA_RECORD_FULL, EMFILE));
    }

    if user_ptr::try_write::<u64>(out, phys.as_u64()).is_err() {
        // Undo in reverse: claim record, then the buffer (run + charge).
        if let Some(claim) = CLAIMS.lock().get_mut(r.id) {
            claim.forget_dma(object_id);
        }
        let _ = shared::close_unseen(handle);
        return Err(refuse(r, reason::DMA_FAULT, EFAULT));
    }

    report::record(
        slot,
        &r.info,
        method::DMA,
        true,
        reason::DMA_ALLOCATED | (((pages & 0xFFFF) as u32) << 8),
    );
    Ok(handle)
}

/// Drop the claim's records of buffers that no longer exist. The registry is
/// consulted with the claim lock released (lock order: `CLAIMS` is never held
/// across another subsystem).
fn prune_records(r: &Resolved) {
    let records = match CLAIMS.lock().get(r.id) {
        Some(claim) if claim.generation == r.claim.generation => claim.dma,
        _ => return,
    };
    let dead: Vec<u64> = records
        .iter()
        .flatten()
        // A quarantined run is kept on purpose until the claim is released.
        .filter(|record| !record.quarantined)
        .map(|record| record.object_id)
        .filter(|id| !shared::is_live(*id))
        .collect();
    if dead.is_empty() {
        return;
    }
    if let Some(claim) = CLAIMS.lock().get_mut(r.id) {
        for id in dead {
            claim.forget_dma(id);
        }
    }
}

/// Return a pool run to the free bitmap without a buffer wrapper (the
/// create-from-frames path already does this itself on failure).
fn free_run(phys: PhysAddr, pages: u64) {
    for page in 0..pages {
        crate::mem::free_frame(PhysAddr::new(phys.as_u64() + page * PAGE));
    }
}
