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

/// Bytes per mapped page.
const PAGE: u64 = 4096;
/// Largest single buffer a process may create.
pub const MAX_BUFFER_SIZE: u64 = 64 << 20;
/// Largest number of live buffers in the kernel registry.
pub const MAX_BUFFERS: usize = 256;
/// Per-process byte quota (section 9's metering, applied to buffer memory).
pub const MAX_BUFFER_BYTES_PER_PROCESS: u64 = 8 << 20;
/// Per-process live-buffer quota.
pub const MAX_BUFFERS_PER_PROCESS: u64 = 64;

/// Creation flags (section 10). The numeric values are kernel-internal; a
/// syscall ABI maps its own constants onto them.
pub mod flags {
    /// The holder may read through a mapping.
    pub const READ: u32 = 1 << 0;
    /// The holder may write through a mapping.
    pub const WRITE: u32 = 1 << 1;
    /// Never map the buffer into any other task: only the creator gets a
    /// mapping, so shared material (key material, DMA targets) stays out of
    /// client address spaces.
    pub const SHARE_ONLY: u32 = 1 << 2;
    /// Request executable mappings. Denied by default: the kernel has no W^X
    /// story for shared memory yet.
    pub const EXECUTABLE: u32 = 1 << 3;
    /// Pin the frames for DMA. Recorded now; used when drivers grow a DMA
    /// path that needs stable physical addresses.
    pub const PINNED: u32 = 1 << 4;
    /// Every defined flag bit; unknown bits are rejected.
    pub const ALL: u32 = READ | WRITE | SHARE_ONLY | EXECUTABLE | PINNED;
}

/// Why a shared-buffer operation failed. Messages are user-facing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The handle is unused or out of range.
    InvalidHandle,
    /// The handle exists but does not name a buffer.
    WrongKind,
    /// The handle lacks the right the operation needs.
    MissingRight,
    /// The process is holding the maximum number of handles.
    NoFreeHandle,
    /// No task exists in the current slot.
    BadTask,
    /// A flag bit is not defined.
    BadFlags,
    /// The size is zero or over [`MAX_BUFFER_SIZE`].
    BadSize,
    /// `EXECUTABLE` was requested; executable shared memory is denied.
    ExecutableDenied,
    /// The kernel buffer registry is full.
    RegistryFull,
    /// The process is over its buffer byte or count quota.
    Quota,
    /// The creator's *user* is over its per-uid kernel-memory quota (issue
    /// #103).
    UserQuota,
    /// No frames were available for the buffer.
    OutOfMemory,
    /// A page could not be mapped into the address space.
    MapFailed,
    /// A `SHARE_ONLY` buffer cannot be mapped into this task.
    ShareOnly,
    /// The buffer is already gone (a stale object id).
    NotFound,
    /// A buffer descriptor's `offset`/`len` does not fit the buffer.
    BadDescriptor,
    /// A fence sequence went backwards.
    StaleSequence,
    /// The deadline passed before the fence sequence was submitted.
    TimedOut,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::InvalidHandle => "that Messenger handle does not exist",
            Error::WrongKind => "that handle does not name a shared buffer",
            Error::MissingRight => "this handle does not grant the right to use the buffer",
            Error::NoFreeHandle => "the process is holding too many Messenger handles",
            Error::BadTask => "no task exists in this slot",
            Error::BadFlags => "unknown shared-buffer flags were requested",
            Error::BadSize => "a shared buffer must be between 1 byte and the size limit",
            Error::ExecutableDenied => "executable shared memory is denied by default",
            Error::RegistryFull => "the kernel shared-buffer registry is full",
            Error::Quota => "this process is over its shared-buffer quota",
            Error::UserQuota => "this user is over its shared-buffer memory quota",
            Error::OutOfMemory => "there is not enough free memory for this buffer",
            Error::MapFailed => "the buffer could not be mapped into this address space",
            Error::ShareOnly => "this buffer is share-only and is not mapped into this process",
            Error::NotFound => "that shared buffer no longer exists",
            Error::BadDescriptor => "the buffer descriptor's range does not fit the buffer",
            Error::StaleSequence => "the fence sequence is older than the last submitted one",
            Error::TimedOut => "the deadline passed before the fence sequence was submitted",
        }
    }
}

/// Translate a handle-table error into the shared-buffer vocabulary.
fn from_handles(error: handles::Error) -> Error {
    match error {
        handles::Error::NoFreeHandle => Error::NoFreeHandle,
        handles::Error::InvalidHandle => Error::InvalidHandle,
        handles::Error::MissingRight => Error::MissingRight,
        handles::Error::BadTask => Error::BadTask,
        // A per-uid handle-quota refusal is the same user-facing condition as
        // the per-process cap (issue #103).
        handles::Error::Quota => Error::NoFreeHandle,
    }
}

/// One buffer's frame run mapped into one task's address space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Mapping {
    /// Task the mapping belongs to (mappings are per slot, not per PML4: two
    /// `CLONE_VM` threads currently meter and map independently).
    slot: usize,
    /// PML4 frame the mapping was installed in (the caller's table, active
    /// `CR3` at syscall time).
    table: u64,
    /// First virtual address of the mapping; the buffer is `size` bytes long.
    va: u64,
}

/// One buffer in the kernel registry.
struct Buffer {
    object_id: u64,
    /// Slot that created the buffer and whose quota it is charged against.
    owner: usize,
    /// Creator's uid *at creation time* (issue #103): the per-uid kernel-memory
    /// charge is released against this uid even if the creator later
    /// transitions identity.
    owner_uid: u32,
    /// Page-rounded length in bytes.
    size: u64,
    flags: u32,
    frames: Vec<PhysAddr>,
    /// Live references: handles plus in-flight message transfers.
    refs: u64,
    mappings: Vec<Mapping>,
    /// Highest fence sequence submitted.
    submitted: u64,
    /// Highest fence sequence a waiter has observed.
    waited: u64,
}

/// Per-process buffer accounting.
#[derive(Clone, Copy, Debug, Default)]
struct Use {
    slot: usize,
    bytes: u64,
    buffers: u64,
    fence_waits: u64,
    fence_timeouts: u64,
}

/// The kernel's shared-buffer state. One mutex keeps every counter and the
/// frame run consistent; the lock is leaf-most except for the frame allocator
/// and the handle table, which are always taken after it.
#[derive(Default)]
struct Registry {
    buffers: Vec<Buffer>,
    uses: Vec<Use>,
    fences_submitted: u64,
    fence_waits: u64,
    fence_timeouts: u64,
    handoffs: u64,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    buffers: Vec::new(),
    uses: Vec::new(),
    fences_submitted: 0,
    fence_waits: 0,
    fence_timeouts: 0,
    handoffs: 0,
});
/// Buffer ids start at 1 so no handle ever carries object id 0.
static NEXT_BUFFER_ID: AtomicU64 = AtomicU64::new(1);
/// Producers wake fence waiters through this queue; wakeups are advisory, so
/// every waiter re-checks its own buffer's counter.
static FENCES: WaitQueue = WaitQueue::new(WaitKind::Sleep);

/// A snapshot of the registry's counters, for `msg_stats` and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Live buffers in the registry.
    pub buffers: u64,
    /// Bytes across live buffers.
    pub bytes: u64,
    /// Live mappings across all tasks.
    pub mappings: u64,
    /// Cumulative `fence_submit` calls.
    pub fences_submitted: u64,
    /// Cumulative `fence_wait` calls that had to park.
    pub fence_waits: u64,
    /// Cumulative waits that hit their deadline.
    pub fence_timeouts: u64,
    /// Fence sequences submitted but not yet observed by a waiter.
    pub outstanding_fences: u64,
    /// Buffer descriptors delivered to a receiver without copying data.
    pub handoffs: u64,
}

/// One process's buffer accounting, for `msg_stats` and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessStats {
    /// Bytes charged to the process.
    pub bytes: u64,
    /// Buffers the process created.
    pub buffers: u64,
    /// Fence waits that had to park.
    pub fence_waits: u64,
    /// Fence waits that hit their deadline.
    pub fence_timeouts: u64,
    /// That process's submissions not yet observed by a waiter.
    pub outstanding_fences: u64,
}

/// A buffer's live state, returned by [`info`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferInfo {
    pub size: u64,
    pub flags: u32,
    /// Backing frames.
    pub frames: u64,
    /// Live references (handles plus in-flight messages).
    pub refs: u64,
    /// Mappings installed in task address spaces.
    pub mappings: u64,
    /// Highest fence sequence submitted.
    pub submitted: u64,
    /// Highest fence sequence observed by a waiter.
    pub waited: u64,
}

/// Borrow (creating on first use) the accounting record for `slot`.
fn use_of(registry: &mut Registry, slot: usize) -> &mut Use {
    if let Some(index) = registry.uses.iter().position(|used| used.slot == slot) {
        return &mut registry.uses[index];
    }
    registry.uses.push(Use {
        slot,
        ..Use::default()
    });
    let last = registry.uses.len() - 1;
    &mut registry.uses[last]
}

/// Release `bytes` from `slot`'s per-process accounting and `uid`'s per-uid
/// kernel-memory quota (buffer destroy or a failed create).
fn release_quota(registry: &mut Registry, slot: usize, uid: u32, bytes: u64) {
    let used = use_of(registry, slot);
    used.bytes = used.bytes.saturating_sub(bytes);
    used.buffers = used.buffers.saturating_sub(1);
    quota::release(uid, Resource::KernelMemory, bytes);
}

/// Page rounded-up length, or `None` on overflow.
fn round_up(size: u64) -> Option<u64> {
    size.checked_add(PAGE - 1).map(|value| value & !(PAGE - 1))
}

/// Page-table flags for a buffer's creation flags.
fn map_flags(flags: u32) -> PageTableFlags {
    let mut prot = Prot(0);
    if flags & flags::READ != 0 {
        prot = prot | Prot::READ;
    }
    if flags & flags::WRITE != 0 {
        prot = prot | Prot::WRITE;
    }
    if !prot.has_read() && !prot.has_write() {
        // A flagless buffer still needs a usable mapping.
        prot = Prot::READ | Prot::WRITE;
    }
    mem::prot_flags(prot)
}

/// Undo a (possibly partial) mapping of `[va, va + pages)` in `table`: drop
/// the mapped leaves, reclaim the page tables they leave empty, and recycle the
/// virtual range. `mapped_end` is the end of what was actually mapped.
fn discard_range(table: PhysAddr, va: u64, mapped_end: u64, pages: u64) {
    mem::unmap_range(table, va, mapped_end);
    mem::reclaim_empty_tables(table, va, va + pages * PAGE);
    super::shared_va::release(va, pages);
}

/// Unmap every page of `mapping`; each 4 KiB leaf loses its frame reference.
fn unmap_mapping(mapping: &Mapping, size: u64) {
    discard_range(
        PhysAddr::new(mapping.table),
        mapping.va,
        mapping.va + size,
        size / PAGE,
    );
}

/// Drop the calling task's mapping for `buffer`, if it has one.
fn unmap_slot(buffer: &mut Buffer, slot: usize) {
    if let Some(index) = buffer.mappings.iter().position(|m| m.slot == slot) {
        let mapping = buffer.mappings.remove(index);
        unmap_mapping(&mapping, buffer.size);
    }
}

/// Take one allocator reference per frame for a new registry reference. The
/// initial allocation in [`create`] already provides the first reference;
/// mappings hold their own. Undoes a partial acquisition on failure.
fn share_frames(frames: &[PhysAddr]) -> bool {
    for (shared, frame) in frames.iter().enumerate() {
        if !mem::share_frame(*frame) {
            for previous in &frames[..shared] {
                mem::free_frame(*previous);
            }
            return false;
        }
    }
    true
}

/// Drop one allocator reference per frame for a registry reference going away
/// (a handle closing, a queued message being discarded, or teardown).
fn free_frames(frames: &[PhysAddr]) {
    for frame in frames {
        mem::free_frame(*frame);
    }
}

/// Free a buffer's frames and quota once its last reference is gone.
///
/// Every mapping still recorded is unmapped here, releasing that mapping's
/// allocator references. `with_refs` releases the allocator references of the
/// outstanding registry references too: normal destruction runs at `refs == 0`
/// (`close`/`release` already released their own), while [`reset`] tears down
/// with live handles, so it must release them explicitly.
fn destroy_buffer(registry: &mut Registry, index: usize, with_refs: bool) {
    let buffer = registry.buffers.remove(index);
    for mapping in &buffer.mappings {
        unmap_mapping(mapping, buffer.size);
    }
    if with_refs {
        for _ in 0..buffer.refs {
            free_frames(&buffer.frames);
        }
    }
    release_quota(registry, buffer.owner, buffer.owner_uid, buffer.size);
}

/// Allocate `pages` zeroed frames, releasing what was allocated on failure.
fn alloc_frames(pages: u64) -> Option<Vec<PhysAddr>> {
    let mut frames = Vec::with_capacity(pages as usize);
    for _ in 0..pages {
        match mem::alloc_zeroed_frame() {
            Some(phys) => frames.push(phys),
            None => {
                for frame in frames {
                    mem::free_frame(frame);
                }
                return None;
            }
        }
    }
    Some(frames)
}

/// Resolve `handle` to the buffer `object_id`, checking kind and rights.
fn object_of(handle: u64, required: u32) -> Result<u64, Error> {
    let entry = handles::get(handle).map_err(from_handles)?;
    if entry.kind != HandleKind::Buffer {
        return Err(Error::WrongKind);
    }
    if entry.rights & required != required {
        return Err(Error::MissingRight);
    }
    Ok(entry.object_id)
}

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
        frames: buffer.frames.len() as u64,
        refs: buffer.refs,
        mappings: buffer.mappings.len() as u64,
        submitted: buffer.submitted,
        waited: buffer.waited,
    })
}

/// Record that everything up to `sequence` in the buffer is ready for readers.
///
/// Sequences must not go backwards: a submit older than the current head is
/// [`Error::StaleSequence`], a repeated submit is a no-op. Submitting wakes
/// every fence waiter (advisory; each re-checks its own buffer).
pub fn fence_submit(handle: u64, sequence: u64) -> Result<(), Error> {
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
        if sequence < buffer.submitted {
            return Err(Error::StaleSequence);
        }
        buffer.submitted = sequence;
        registry.fences_submitted += 1;
    }
    FENCES.notify_all();
    Ok(())
}

/// Wait until the buffer's fence head is at least `sequence`.
///
/// Parks on [`FENCES`] until a producer's [`fence_submit`] passes the sequence
/// or `deadline` (absolute PIT ticks) passes. A submit that lands between the
/// check and the park still resolves the wait: every wakeup re-checks the
/// counter.
pub fn fence_wait(handle: u64, sequence: u64, deadline: Option<u64>) -> Result<(), Error> {
    let object_id = object_of(handle, rights::CALL)?;
    let me = task::current();
    let mut counted = false;
    loop {
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
                return Ok(());
            }
            if !counted {
                counted = true;
                registry.fence_waits += 1;
                use_of(&mut registry, me).fence_waits += 1;
            }
        }
        let reason = FENCES.wait(me, deadline);
        if reason != WakeReason::TimedOut {
            continue;
        }
        // Re-check once more: a submit may have won the race with the
        // deadline sweep.
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
            return Ok(());
        }
        registry.fence_timeouts += 1;
        use_of(&mut registry, me).fence_timeouts += 1;
        return Err(Error::TimedOut);
    }
}

/// Aggregate counters across every live buffer.
pub fn stats() -> Stats {
    let registry = REGISTRY.lock();
    let mut stats = Stats {
        buffers: registry.buffers.len() as u64,
        ..Default::default()
    };
    for buffer in &registry.buffers {
        stats.bytes += buffer.size;
        stats.mappings += buffer.mappings.len() as u64;
        stats.outstanding_fences += buffer.submitted.saturating_sub(buffer.waited);
    }
    stats.fences_submitted = registry.fences_submitted;
    stats.fence_waits = registry.fence_waits;
    stats.fence_timeouts = registry.fence_timeouts;
    stats.handoffs = registry.handoffs;
    stats
}

/// Per-process accounting for `slot`.
pub fn process_stats(slot: usize) -> ProcessStats {
    let registry = REGISTRY.lock();
    let used = registry.uses.iter().find(|used| used.slot == slot);
    let mut stats = ProcessStats {
        bytes: used.map(|used| used.bytes).unwrap_or(0),
        buffers: used.map(|used| used.buffers).unwrap_or(0),
        fence_waits: used.map(|used| used.fence_waits).unwrap_or(0),
        fence_timeouts: used.map(|used| used.fence_timeouts).unwrap_or(0),
        outstanding_fences: 0,
    };
    for buffer in &registry.buffers {
        if buffer.owner == slot {
            stats.outstanding_fences += buffer.submitted.saturating_sub(buffer.waited);
        }
    }
    stats
}

/// Drop every buffer, unmapping its frames, and reset the counters and fence
/// waiters. Process teardown and test isolation (the caller must run before the
/// affected address spaces are torn down).
pub fn reset() {
    let mut registry = REGISTRY.lock();
    while !registry.buffers.is_empty() {
        let last = registry.buffers.len() - 1;
        // Handles may still be open: release their allocator references too.
        destroy_buffer(&mut registry, last, true);
    }
    registry.uses.clear();
    registry.fences_submitted = 0;
    registry.fence_waits = 0;
    registry.fence_timeouts = 0;
    registry.handoffs = 0;
    drop(registry);
    FENCES.notify_all();
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
}
