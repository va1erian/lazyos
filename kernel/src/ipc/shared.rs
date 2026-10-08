//! Shared buffers (issue #67).
//!
//! `docs/messenger.md` sections 4, 7.1 and 10: a shared buffer is a page-aligned
//! run of frames that can be mapped read/write into several address spaces at
//! once. The kernel keeps those frames in the `REGISTRY` keyed by the
//! `object_id` carried by `HandleKind::Buffer` handles, so a handle number
//! stays a small per-process integer and the frames outlive any single holder.
//!
//! Lifetime is reference-counted. Every handle holds one reference; every
//! in-flight message that carries the buffer holds one, taken by [`retain`] /
//! [`retain_descriptor`] when the parcel is queued and turned into the
//! receiver's handle by [`attach`] on delivery. Each registry reference and
//! each mapping holds one allocator reference per frame, so a frame returns to
//! the free pool exactly when its last reference (in flight, mapped, or held by
//! a handle) goes away.
//!
//! Mapping is per task slot: [`create`] maps the buffer into the creator and
//! [`map`] adds a mapping for the calling task. [`close`] unmaps the calling
//! task's mapping and drops its reference. Mappings never copy: every mapping
//! of a buffer points at the same physical frames, which is what makes surface
//! handoff and DMA rings copy-free (`stats().handoffs` counts deliveries done
//! that way, and the zero-copy test checks frame identity directly).
//!
//! A driver's `dma_alloc(SHARE_ONLY)` buffer ([`create_from_frames`]) is
//! mapped for its creator only: the kernel refuses [`map`] for any other task,
//! so a DMA target can be handed to a client without entering its address
//! space. Ordinary buffers have no flags: every mapping is read/write.
//!
//! Locking: `REGISTRY` is held while frame mapping runs (`REGISTRY` -> frame
//! allocator) and while a handle is opened (`REGISTRY` -> handle table); those
//! orders are never inverted.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;
use x86_64::structures::paging::PageTableFlags;
use x86_64::{PhysAddr, VirtAddr};

use crate::ipc::credentials;
use crate::ipc::handles::{self, rights, HandleKind};
use crate::mem;
use crate::mem::vma::Prot;
use crate::quota::{self, Resource};
use crate::task;

mod dma;
mod registry;
mod stats;
mod types;

pub use dma::{create_from_frames, is_live, DmaOwner};
use registry::*;
pub use stats::*;
pub use types::*;

/// Create a buffer of `size` bytes and return a `Buffer` handle in the calling
/// process.
///
/// Frames are zeroed and mapped read/write into the caller's address space on
/// a fresh virtual range; [`map`] returns that address. The creator holds one
/// reference; handles opened by receivers hold the references that [`attach`]
/// converts from in-flight messages.
pub fn create(size: u64) -> Result<u64, Error> {
    let size = round_up(size).filter(|bytes| *bytes > 0 && *bytes <= max_bytes_per_process());
    let Some(size) = size else {
        return Err(Error::BadSize);
    };
    let slot = task::current();
    let uid = credentials::of(slot).uid;
    let pages = size / PAGE;

    let mut registry = REGISTRY.lock();
    if registry.buffers.len() >= MAX_BUFFERS {
        return Err(Error::RegistryFull);
    }
    {
        let used = use_of(&mut registry, slot);
        if used.buffers + 1 > MAX_BUFFERS_PER_PROCESS
            || used.bytes.saturating_add(size) > max_bytes_per_process()
        {
            return Err(Error::Quota);
        }
    }
    // Per-uid aggregate (issue #103): the frames are kernel memory charged to
    // the creator's user. Charge before reserving so a refusal has nothing to
    // unwind; every later failure path releases through `release_quota`.
    if quota::charge(uid, Resource::KernelMemory, size).is_err() {
        return Err(Error::UserQuota);
    }
    {
        // Reserve the quota before allocating so a failure cannot leak it.
        let used = use_of(&mut registry, slot);
        used.bytes += size;
        used.buffers += 1;
    }
    let Some(frames) = alloc_frames(pages) else {
        release_quota(&mut registry, slot, uid, size);
        return Err(Error::OutOfMemory);
    };

    // Map the creator's view. Each mapping holds one allocator reference per
    // frame, so the allocator refcount stays `refs + mappings`.
    let table = mem::kernel_table();
    let mut mappings = Vec::new();
    let va = super::shared_va::alloc(pages);
    for (index, frame) in frames.iter().enumerate() {
        let at = va + index as u64 * PAGE;
        if !mem::share_frame(*frame) {
            // Undo the mapped prefix's mapping references, then return every
            // frame's allocation reference.
            discard_range(table, va, at, pages);
            free_frames(&frames);
            release_quota(&mut registry, slot, uid, size);
            return Err(Error::MapFailed);
        }
        if !mem::map_page_in(table, VirtAddr::new(at), *frame, map_flags()) {
            // Undo this share and the mapped prefix, then return every
            // frame's allocation reference.
            mem::free_frame(*frame);
            discard_range(table, va, at, pages);
            free_frames(&frames);
            release_quota(&mut registry, slot, uid, size);
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
            // Undo the creator's mapping (if any), then the would-be registry
            // reference's allocator references.
            for mapping in &mappings {
                unmap_mapping(mapping, size);
            }
            free_frames(&frames);
            release_quota(&mut registry, slot, uid, size);
            return Err(from_handles(error));
        }
    };
    registry.buffers.push(Buffer {
        object_id,
        owner: slot,
        owner_uid: uid,
        size,
        share_only: false,
        frames,
        refs: 1,
        mappings,
        dma: None,
    });
    Ok(handle)
}

/// Map the buffer `handle` names into the calling task and return its address.
///
/// The mapping is idempotent per task: a second call returns the recorded
/// address. A share-only DMA buffer is refused for every task except the
/// creator (whose mapping creation already installed), so a received handle
/// can never be mapped by a client.
pub fn map(handle: u64) -> Result<u64, Error> {
    let object_id = object_of(handle, rights::CALL)?;
    let slot = task::current();
    let mut registry = REGISTRY.lock();
    let Some(index) = registry
        .buffers
        .iter()
        .position(|buffer| buffer.object_id == object_id)
    else {
        return Err(Error::NotFound);
    };
    let buffer = &mut registry.buffers[index];
    if let Some(mapping) = buffer.mappings.iter().find(|mapping| mapping.slot == slot) {
        return Ok(mapping.va);
    }
    if buffer.share_only {
        return Err(Error::ShareOnly);
    }
    let table = mem::kernel_table();
    let va = super::shared_va::alloc(buffer.size / PAGE);
    let mut mapped = 0u64;
    for (index, frame) in buffer.frames.iter().enumerate() {
        let at = va + index as u64 * PAGE;
        if !mem::share_frame(*frame) {
            discard_range(table, va, at, buffer.size / PAGE);
            return Err(Error::MapFailed);
        }
        if !mem::map_page_in(table, VirtAddr::new(at), *frame, map_flags()) {
            // Undo this frame's share and the pages mapped before it.
            mem::free_frame(*frame);
            discard_range(table, va, at, buffer.size / PAGE);
            return Err(Error::MapFailed);
        }
        mapped += 1;
    }
    debug_assert_eq!(mapped, buffer.size / PAGE);
    buffer.mappings.push(Mapping {
        slot,
        table: table.as_u64(),
        va,
    });
    Ok(va)
}

/// Close a buffer handle held by the calling task.
///
/// Closing unmaps this task's mapping, drops one reference, and frees the
/// frames when the last reference (another task's handle, or an in-flight
/// message) goes away.
pub fn close(handle: u64) -> Result<(), Error> {
    close_for(task::current(), handle, true)
}

/// [`close`] for a DMA buffer the device was never told about (a failed
/// `dma_alloc`): dropping the last reference then needs no device quiesce.
pub fn close_unseen(handle: u64) -> Result<(), Error> {
    close_impl(task::current(), handle, true, false)
}

/// [`close`] for a handle in `slot`'s table.
///
/// `unmap` says whether the slot's mapping may be unmapped now. Task teardown
/// passes `false` when another live task still shares the address space (a
/// `CLONE_VM` sibling): the mapping stays installed, and [`teardown_task`]
/// removes it (without releasing any frame reference a second time -- this
/// call already released the handle's own reference) once the last sharer
/// goes.
fn close_for(slot: usize, handle: u64, unmap: bool) -> Result<(), Error> {
    close_impl(slot, handle, unmap, true)
}

fn close_impl(slot: usize, handle: u64, unmap: bool, seen: bool) -> Result<(), Error> {
    let entry = handles::get_for_task(slot, handle).map_err(from_handles)?;
    if entry.kind != HandleKind::Buffer {
        return Err(Error::WrongKind);
    }
    let object_id = entry.object_id;
    handles::close_for_task(slot, handle).map_err(from_handles)?;
    let mut registry = REGISTRY.lock();
    let Some(index) = registry
        .buffers
        .iter()
        .position(|buffer| buffer.object_id == object_id)
    else {
        return Ok(());
    };
    let quarantined = before_last_drop(&registry.buffers[index], Some(slot), seen);
    if unmap {
        unmap_slot(&mut registry.buffers[index], slot);
    }
    let buffer = &mut registry.buffers[index];
    // Drop this handle's references: its allocator reference per frame (kept
    // when the run is quarantined), then its place in the registry count.
    if !quarantined {
        free_frames(&buffer.frames);
    }
    buffer.refs = buffer.refs.saturating_sub(1);
    if buffer.refs == 0 {
        destroy_buffer(&mut registry, index, false, quarantined);
    }
    Ok(())
}

/// Close whichever handle in `slot` names buffer `object_id`, if any (issue
/// #241). Device teardown knows its DMA buffers by object id, not by a handle
/// number that may have been moved away or reused, so it closes by identity.
/// Returns whether a handle was closed.
pub fn close_owned_for_task(slot: usize, object_id: u64, unmap: bool) -> bool {
    for (handle, entry) in handles::entries_for_task(slot) {
        if entry.kind == HandleKind::Buffer && entry.object_id == object_id {
            let _ = close_for(slot, handle, unmap);
            return true;
        }
    }
    false
}

/// Close every buffer handle a reclaimed task slot still holds, then drop the
/// mappings and accounting that belonged to its address space.
///
/// `table` is the slot's PML4 and `table_shared` says whether another live
/// task still uses it. Called *before* the address space is freed: a mapping
/// left in the registry would otherwise be unmapped later through a freed (and
/// possibly reused) page-table frame, and the frames it references would be
/// released twice.
pub fn teardown_task(slot: usize, table: u64, table_shared: bool) {
    for (handle, entry) in handles::entries_for_task(slot) {
        if entry.kind == HandleKind::Buffer {
            let _ = close_for(slot, handle, !table_shared);
        }
    }
    let mut registry = REGISTRY.lock();
    registry.uses.retain(|used| used.slot != slot);
    if table_shared {
        return;
    }
    // Last user of the address space: remove every mapping still recorded
    // against `table`. Its frame references were already released above, by
    // the `close_for(slot, handle, false)` call that ran while this slot's own
    // handles were still open (`unmap = false` deferred only the *mapping*
    // removal, not the reference release), so `unmap_mapping` here only tears
    // down the mapping itself and cannot double-release or leak a frame
    // reference.
    for index in 0..registry.buffers.len() {
        let buffer = &mut registry.buffers[index];
        while let Some(at) = buffer.mappings.iter().position(|m| m.table == table) {
            let mapping = buffer.mappings.remove(at);
            unmap_mapping(&mapping, buffer.size);
        }
    }
}

/// Take the message reference for a buffer a queued parcel shares: the
/// sender keeps its own handle and mapping, and this reference lives until
/// [`attach`] turns it into the receiver's handle (or the message is
/// dropped, [`release`]).
pub fn retain(object_id: u64) -> Result<(), Error> {
    let mut registry = REGISTRY.lock();
    let Some(buffer) = registry
        .buffers
        .iter_mut()
        .find(|buffer| buffer.object_id == object_id)
    else {
        return Err(Error::NotFound);
    };
    if !share_frames(&buffer.frames) {
        return Err(Error::NotFound);
    }
    buffer.refs += 1;
    Ok(())
}

/// Drop a queued message's reference to a buffer (queue cleared, peer died,
/// or delivery failed before the handle was installed).
pub fn release(object_id: u64) {
    let mut registry = REGISTRY.lock();
    let Some(index) = registry
        .buffers
        .iter()
        .position(|buffer| buffer.object_id == object_id)
    else {
        return;
    };
    let quarantined = before_last_drop(&registry.buffers[index], None, true);
    let buffer = &mut registry.buffers[index];
    if !quarantined {
        free_frames(&buffer.frames);
    }
    buffer.refs = buffer.refs.saturating_sub(1);
    if buffer.refs == 0 {
        destroy_buffer(&mut registry, index, false, quarantined);
    }
}

/// Install a delivered buffer into the calling (receiving) task's handle
/// table, converting the message's in-flight reference into the new handle.
/// No data is copied, so the handoff counter advances.
pub fn attach(object_id: u64, rights: u32) -> Result<u64, Error> {
    let mut registry = REGISTRY.lock();
    if !registry
        .buffers
        .iter()
        .any(|buffer| buffer.object_id == object_id)
    {
        return Err(Error::NotFound);
    }
    registry.handoffs += 1;
    handles::open(HandleKind::Buffer, rights, object_id).map_err(from_handles)
}

/// Live state of the buffer `handle` names.
pub fn info(handle: u64) -> Result<BufferInfo, Error> {
    let object_id = object_of(handle, 0)?;
    let registry = REGISTRY.lock();
    let Some(buffer) = registry
        .buffers
        .iter()
        .find(|buffer| buffer.object_id == object_id)
    else {
        return Err(Error::NotFound);
    };
    Ok(BufferInfo {
        size: buffer.size,
        dma: buffer.dma.is_some(),
        frames: buffer.frames.len() as u64,
        refs: buffer.refs,
        mappings: buffer.mappings.len() as u64,
    })
}

/// Test-harness hooks (issue #62), compiled only with `LAZYOS_TESTS=1`.
#[cfg(lazyos_tests)]
pub mod harness {
    use super::*;

    /// The backing frames of the buffer `handle` names, for tests that check
    /// contiguity, zeroing and frame identity (issue #241).
    pub fn frames(handle: u64) -> Result<Vec<PhysAddr>, Error> {
        let object_id = object_of(handle, 0)?;
        let registry = REGISTRY.lock();
        registry
            .buffers
            .iter()
            .find(|buffer| buffer.object_id == object_id)
            .map(|buffer| buffer.frames.clone())
            .ok_or(Error::NotFound)
    }
}
