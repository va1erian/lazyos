//! Shared buffers and fences (issue #67).
//!
//! `docs/messenger.md` sections 4, 7.1 and 10: a shared buffer is a page-aligned
//! run of frames that can be mapped into several address spaces at once. The
//! kernel keeps those frames in the `REGISTRY` keyed by the `object_id` carried
//! by `HandleKind::Buffer` handles, so a handle number stays a small per-process
//! integer and the frames outlive any single holder.
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
//! `SHARE_ONLY` withholds mappings from everyone except the creator's initial
//! one: the kernel refuses [`map`] for any other task, so key material can be
//! handed to a service without ever entering its address space.
//!
//! Fences are per-buffer monotonic counters: a producer calls [`fence_submit`]
//! after writing, a consumer calls [`fence_wait`] before reading. Waiters park
//! on [`FENCES`] and honor deadlines, so ordering costs no extra copies.
//!
//! Locking: `REGISTRY` is held while frame mapping runs (`REGISTRY` -> frame
//! allocator) and while a handle is opened (`REGISTRY` -> handle table); those
//! orders are never inverted. `FENCES` is notified only after `REGISTRY` is
//! released, matching the queue-then-task order in `task::wait`.

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
use crate::task::wait::WaitQueue;
use crate::task::{self, WaitKind, WakeReason};

mod dma;
mod fences;
mod registry;
mod types;

pub use dma::{create_from_frames, is_live, DmaOwner};
pub use fences::*;
use registry::*;
pub use types::*;

/// Create a buffer of `size` bytes and return a `Buffer` handle in the calling
/// process.
///
/// Frames are zeroed and mapped (read/write, or a flagless read/write default)
/// into the caller's address space on a fresh virtual range; [`map`] returns
/// that address. A `SHARE_ONLY` buffer is mapped for the creator only: [`map`]
/// refuses every other task. The creator holds one reference; handles opened
/// by receivers hold the references that [`attach`] converts from in-flight
/// messages.
pub fn create(size: u64, flags: u32) -> Result<u64, Error> {
    if flags & !flags::ALL != 0 {
        return Err(Error::BadFlags);
    }
    if flags & flags::EXECUTABLE != 0 {
        return Err(Error::ExecutableDenied);
    }
    let size = round_up(size).filter(|bytes| *bytes > 0 && *bytes <= MAX_BUFFER_SIZE);
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
            || used.bytes.saturating_add(size) > MAX_BUFFER_BYTES_PER_PROCESS
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

    // Map the creator's view, including for a `SHARE_ONLY` buffer: the creator
    // is the one process that may ever hold a mapping. Each mapping holds one
    // allocator reference per frame, so the allocator refcount stays
    // `refs + mappings`.
    let table = mem::kernel_table();
    let mut mappings = Vec::new();
    let va = super::shared_va::alloc(pages);
    for (index, frame) in frames.iter().enumerate() {
        let at = va + index as u64 * PAGE;
        if !mem::share_frame(*frame) {
            // Undo the mapped prefix; the never-shared tail keeps only its
            // allocation reference and is freed directly.
            discard_range(table, va, at, pages);
            for remaining in &frames[index..] {
                mem::free_frame(*remaining);
            }
            release_quota(&mut registry, slot, uid, size);
            return Err(Error::MapFailed);
        }
        if !mem::map_page_in(table, VirtAddr::new(at), *frame, map_flags(flags)) {
            // Undo this share, the mapped prefix, and the unshared tail.
            mem::free_frame(*frame);
            discard_range(table, va, at, pages);
            for remaining in &frames[index + 1..] {
                mem::free_frame(*remaining);
            }
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
        flags,
        frames,
        refs: 1,
        mappings,
        submitted: 0,
        waited: 0,
        dma: None,
    });
    Ok(handle)
}

/// Map the buffer `handle` names into the calling task and return its address.
///
/// The mapping is idempotent per task: a second call returns the recorded
/// address. A `SHARE_ONLY` buffer is refused for every task except the creator
/// (whose mapping creation already installed), so a received handle can never
/// be mapped by a client.
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
    if buffer.flags & flags::SHARE_ONLY != 0 {
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
        if !mem::map_page_in(table, VirtAddr::new(at), *frame, map_flags(buffer.flags)) {
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

/// [`close`] for a handle in `slot`'s table.
///
/// `unmap` says whether the slot's mapping may be unmapped now. Task teardown
/// passes `false` when another live task still shares the address space (a
/// `CLONE_VM` sibling): the mapping stays installed, and [`teardown_task`]
/// removes it (without releasing any frame reference a second time -- this
/// call already released the handle's own reference) once the last sharer
/// goes.
fn close_for(slot: usize, handle: u64, unmap: bool) -> Result<(), Error> {
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
    if unmap {
        unmap_slot(&mut registry.buffers[index], slot);
    }
    let buffer = &mut registry.buffers[index];
    // Drop this handle's references: its allocator reference per frame, then
    // its place in the registry count.
    free_frames(&buffer.frames);
    buffer.refs = buffer.refs.saturating_sub(1);
    if buffer.refs == 0 {
        destroy_buffer(&mut registry, index, false);
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

/// Take the message reference for a buffer moved inside a parcel's handle
/// list: the sender's handle is closed once the message is queued, and the
/// message keeps the frame run alive until delivery.
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

/// Validate a descriptor's range and take the message reference for it.
///
/// Called when a parcel carrying the descriptor is queued; the sender keeps its
/// own handle and mapping, and this reference lives until [`attach`] turns it
/// into the receiver's handle.
pub fn retain_descriptor(object_id: u64, offset: u64, len: u64) -> Result<(), Error> {
    let mut registry = REGISTRY.lock();
    let Some(buffer) = registry
        .buffers
        .iter_mut()
        .find(|buffer| buffer.object_id == object_id)
    else {
        return Err(Error::NotFound);
    };
    let end = offset.checked_add(len).ok_or(Error::BadDescriptor)?;
    if len == 0 || end > buffer.size {
        return Err(Error::BadDescriptor);
    }
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
    let buffer = &mut registry.buffers[index];
    free_frames(&buffer.frames);
    buffer.refs = buffer.refs.saturating_sub(1);
    if buffer.refs == 0 {
        destroy_buffer(&mut registry, index, false);
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
        flags: buffer.flags,
        dma: buffer.dma.is_some(),
        frames: buffer.frames.len() as u64,
        refs: buffer.refs,
        mappings: buffer.mappings.len() as u64,
        submitted: buffer.submitted,
        waited: buffer.waited,
    })
}

/// Test-harness hooks (issue #62), compiled only with `LAZYOS_TESTS=1`.
#[cfg(lazyos_tests)]
pub mod harness {
    use super::*;

    /// Do a single fence check for the current task and, when the sequence is
    /// not there yet, park it on [`FENCES`] without yielding. Returns whether
    /// the wait was already satisfied. The suite uses this to observe the
    /// block/wake path without a running scheduler.
    pub fn park_wait(handle: u64, sequence: u64, deadline: Option<u64>) -> Result<bool, Error> {
        let object_id = object_of(handle, rights::CALL)?;
        {
            let mut registry = REGISTRY.lock();
            let Some(index) = registry
                .buffers
                .iter()
                .position(|buffer| buffer.object_id == object_id)
            else {
                return Err(Error::NotFound);
            };
            let buffer = &mut registry.buffers[index];
            if buffer.submitted >= sequence {
                buffer.waited = buffer.waited.max(sequence);
                return Ok(true);
            }
        }
        FENCES.park(task::current(), deadline);
        Ok(false)
    }

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
