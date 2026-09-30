//! DMA-backed shared buffers (issue #241).
//!
//! A device driver allocates a physically contiguous run from the boot-time DMA
//! pool (`mem::dma`) and wraps it in the ordinary shared-buffer object so it can
//! be passed to a client zero-copy. The buffer is created exactly like
//! [`create`](super::create) — same registry, handle rights, mapping and
//! transfer paths — with two differences:
//!
//! * the frames already exist (the caller allocated them) and are *not* charged
//!   to `KernelMemory`; the driver's `DmaMemory` quota covers them instead, and
//!   [`super::destroy_buffer`] releases it exactly once on the last drop;
//! * the buffer remembers its [`DmaOwner`] so teardown can find it and so the
//!   charge is credited to the uid that allocated it.
//!
//! The per-process buffer *count* cap still applies (bytes are governed by the
//! `DmaMemory` quota, not the 8 MiB shared-buffer byte cap).

use super::*;

/// DMA provenance of a buffer: the device claim that allocated its frames and
/// the uid its `DmaMemory` charge belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DmaOwner {
    /// Device id the claim was for.
    pub device: u16,
    /// Claim generation, so a stale record can never match.
    pub generation: u32,
    /// Creator's uid at allocation time; the `DmaMemory` charge is released
    /// against this uid even if the creator later transitions identity.
    pub uid: u32,
}

/// Create a registry buffer over caller-provided, already-allocated contiguous
/// frames (issue #241). The frames' first reference is the registry's and each
/// creator mapping takes its own, exactly as [`create`](super::create) does.
/// `KernelMemory` is not charged; the per-process buffer count still applies.
///
/// The caller maps the buffer into the calling task and owns the returned
/// handle. Every failure here frees the frames (returning them to the pool) and
/// undoes the count, so the caller need only unwind its own `DmaMemory` charge.
pub fn create_from_frames(
    frames: Vec<PhysAddr>,
    flags: u32,
    uid: u32,
    owner: DmaOwner,
) -> Result<u64, Error> {
    // The caller owns the frames and expects every failure to return them.
    if flags & !flags::ALL != 0 {
        free_frames(&frames);
        return Err(Error::BadFlags);
    }
    if flags & flags::EXECUTABLE != 0 {
        free_frames(&frames);
        return Err(Error::ExecutableDenied);
    }
    if frames.is_empty() {
        return Err(Error::BadSize);
    }
    let size = frames.len() as u64 * PAGE;
    let slot = task::current();

    let mut registry = REGISTRY.lock();
    if !charge_dma_accounting(&mut registry, slot) {
        // The frames are already allocated: return them so the caller's only
        // remaining undo is the `DmaMemory` charge.
        free_frames(&frames);
        return Err(Error::Quota);
    }
    let table = mem::kernel_table();
    let mut mappings = Vec::new();
    let va = crate::ipc::shared_va::alloc(size / PAGE);
    for (index, frame) in frames.iter().enumerate() {
        let at = va + index as u64 * PAGE;
        // Each frame holds its allocation reference plus one per mapping. On
        // failure `discard_range` drops the prefix's mapping references; every
        // frame's allocation reference is then returned to the pool.
        if !mem::share_frame(*frame) {
            discard_range(table, va, at, size / PAGE);
            free_frames(&frames);
            release_dma_accounting(&mut registry, slot);
            return Err(Error::MapFailed);
        }
        if !mem::map_page_in(table, VirtAddr::new(at), *frame, map_flags(flags)) {
            // Drop the mapping reference just taken on this frame.
            mem::free_frame(*frame);
            discard_range(table, va, at, size / PAGE);
            free_frames(&frames);
            release_dma_accounting(&mut registry, slot);
            return Err(Error::MapFailed);
        }
    }
    mappings.push(Mapping {
        slot,
        table: table.as_u64(),
        va,
    });

    let object_id = NEXT_BUFFER_ID.fetch_add(1, Ordering::Relaxed);
    let handle = match handles::open(HandleKind::Buffer, rights::ALL, object_id) {
        Ok(handle) => handle,
        Err(error) => {
            for mapping in &mappings {
                unmap_mapping(mapping, size);
            }
            free_frames(&frames);
            release_dma_accounting(&mut registry, slot);
            return Err(from_handles(error));
        }
    };
    registry.buffers.push(Buffer {
        object_id,
        owner: slot,
        owner_uid: uid,
        size,
        flags,
        frames,
        refs: 1,
        mappings,
        submitted: 0,
        waited: 0,
        dma: Some(owner),
    });
    Ok(handle)
}

/// Whether the buffer `object_id` is still in the registry. The claim's DMA
/// records are pruned against this, outside the claim lock.
pub fn is_live(object_id: u64) -> bool {
    REGISTRY
        .lock()
        .buffers
        .iter()
        .any(|buffer| buffer.object_id == object_id)
}
