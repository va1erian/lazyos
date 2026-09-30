//! The buffer registry and the helpers that account, map and free buffers.

use super::*;

/// One buffer's frame run mapped into one task's address space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Mapping {
    /// Task the mapping belongs to (mappings are per slot, not per PML4: two
    /// `CLONE_VM` threads currently meter and map independently).
    pub(super) slot: usize,
    /// PML4 frame the mapping was installed in (the caller's table, active
    /// `CR3` at syscall time).
    pub(super) table: u64,
    /// First virtual address of the mapping; the buffer is `size` bytes long.
    pub(super) va: u64,
}

/// One buffer in the kernel registry.
pub(super) struct Buffer {
    pub(super) object_id: u64,
    /// Slot that created the buffer and whose quota it is charged against.
    pub(super) owner: usize,
    /// Creator's uid *at creation time* (issue #103): the per-uid kernel-memory
    /// charge is released against this uid even if the creator later
    /// transitions identity.
    pub(super) owner_uid: u32,
    /// Page-rounded length in bytes.
    pub(super) size: u64,
    pub(super) flags: u32,
    pub(super) frames: Vec<PhysAddr>,
    /// Live references: handles plus in-flight message transfers.
    pub(super) refs: u64,
    pub(super) mappings: Vec<Mapping>,
    /// Highest fence sequence submitted.
    pub(super) submitted: u64,
    /// Highest fence sequence a waiter has observed.
    pub(super) waited: u64,
    /// `Some` for a DMA-backed buffer: its frames come from the DMA pool and
    /// its quota is [`Resource::DmaMemory`], not `KernelMemory` (issue #241).
    pub(super) dma: Option<DmaOwner>,
}

/// Per-process buffer accounting.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Use {
    pub(super) slot: usize,
    pub(super) bytes: u64,
    pub(super) buffers: u64,
    pub(super) fence_waits: u64,
    pub(super) fence_timeouts: u64,
}

/// The kernel's shared-buffer state. One mutex keeps every counter and the
/// frame run consistent; the lock is leaf-most except for the frame allocator
/// and the handle table, which are always taken after it.
#[derive(Default)]
pub(super) struct Registry {
    pub(super) buffers: Vec<Buffer>,
    pub(super) uses: Vec<Use>,
    pub(super) fences_submitted: u64,
    pub(super) fence_waits: u64,
    pub(super) fence_timeouts: u64,
    pub(super) handoffs: u64,
}

pub(super) static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    buffers: Vec::new(),
    uses: Vec::new(),
    fences_submitted: 0,
    fence_waits: 0,
    fence_timeouts: 0,
    handoffs: 0,
});
/// Buffer ids start at 1 so no handle ever carries object id 0.
pub(super) static NEXT_BUFFER_ID: AtomicU64 = AtomicU64::new(1);
/// Producers wake fence waiters through this queue; wakeups are advisory, so
/// every waiter re-checks its own buffer's counter.
pub(super) static FENCES: WaitQueue = WaitQueue::new(WaitKind::Sleep);

/// Borrow (creating on first use) the accounting record for `slot`.
pub(super) fn use_of(registry: &mut Registry, slot: usize) -> &mut Use {
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
pub(super) fn release_quota(registry: &mut Registry, slot: usize, uid: u32, bytes: u64) {
    let used = use_of(registry, slot);
    used.bytes = used.bytes.saturating_sub(bytes);
    used.buffers = used.buffers.saturating_sub(1);
    quota::release(uid, Resource::KernelMemory, bytes);
}

/// Give back a DMA buffer's per-process *count* only (issue #241). DMA bytes
/// are metered by [`Resource::DmaMemory`], which the caller released before
/// creation and which [`destroy_buffer`] releases on the last drop.
pub(super) fn release_dma_accounting(registry: &mut Registry, slot: usize) {
    let used = use_of(registry, slot);
    used.buffers = used.buffers.saturating_sub(1);
}

/// Charge `slot` one live buffer for a DMA buffer, enforcing the same
/// per-process count cap as [`create`](super::create). Returns the bytes-free
/// usage that [`release_dma_accounting`] later gives back.
pub(super) fn charge_dma_accounting(registry: &mut Registry, slot: usize) -> bool {
    if registry.buffers.len() >= MAX_BUFFERS {
        return false;
    }
    let used = use_of(registry, slot);
    if used.buffers + 1 > MAX_BUFFERS_PER_PROCESS {
        return false;
    }
    used.buffers += 1;
    true
}

/// Page rounded-up length, or `None` on overflow.
pub(super) fn round_up(size: u64) -> Option<u64> {
    size.checked_add(PAGE - 1).map(|value| value & !(PAGE - 1))
}

/// Page-table flags for a buffer's creation flags.
pub(super) fn map_flags(flags: u32) -> PageTableFlags {
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
pub(super) fn discard_range(table: PhysAddr, va: u64, mapped_end: u64, pages: u64) {
    mem::unmap_range(table, va, mapped_end);
    mem::reclaim_empty_tables(table, va, va + pages * PAGE);
    crate::ipc::shared_va::release(va, pages);
}

/// Unmap every page of `mapping`; each 4 KiB leaf loses its frame reference.
pub(super) fn unmap_mapping(mapping: &Mapping, size: u64) {
    discard_range(
        PhysAddr::new(mapping.table),
        mapping.va,
        mapping.va + size,
        size / PAGE,
    );
}

/// Drop the calling task's mapping for `buffer`, if it has one.
pub(super) fn unmap_slot(buffer: &mut Buffer, slot: usize) {
    if let Some(index) = buffer.mappings.iter().position(|m| m.slot == slot) {
        let mapping = buffer.mappings.remove(index);
        unmap_mapping(&mapping, buffer.size);
    }
}

/// Take one allocator reference per frame for a new registry reference. The
/// initial allocation in [`create`] already provides the first reference;
/// mappings hold their own. Undoes a partial acquisition on failure.
pub(super) fn share_frames(frames: &[PhysAddr]) -> bool {
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
pub(super) fn free_frames(frames: &[PhysAddr]) {
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
pub(super) fn destroy_buffer(
    registry: &mut Registry,
    index: usize,
    with_refs: bool,
    quarantined: bool,
) {
    let buffer = registry.buffers.remove(index);
    for mapping in &buffer.mappings {
        unmap_mapping(mapping, buffer.size);
    }
    if with_refs {
        for _ in 0..buffer.refs {
            free_frames(&buffer.frames);
        }
    }
    // A DMA buffer's bytes are metered by `DmaMemory` against the uid that
    // allocated them; a normal buffer by `KernelMemory`. Either way the charge
    // is released exactly once, here, when the last reference goes.
    match buffer.dma {
        Some(owner) => {
            let used = use_of(registry, buffer.owner);
            used.buffers = used.buffers.saturating_sub(1);
            // A quarantined run keeps its charge until the claim is released.
            if !quarantined {
                quota::release(owner.uid, Resource::DmaMemory, buffer.size);
            }
        }
        None => release_quota(registry, buffer.owner, buffer.owner_uid, buffer.size),
    }
}

/// Before the last reference to a DMA buffer goes (and its run can return to the
/// pool), stop the device that may still be writing it (issue #241). A no-op
/// for ordinary buffers and for a claim that is already gone (release and task
/// death quiesce first).
///
/// `closer` is the task dropping the reference (`None` for a discarded
/// message) and `seen` is false for a buffer the device never learned about
/// (a failed `dma_alloc`). The driver freeing its own buffer stops its device
/// (an explicit free); anyone else's last drop must not stop a running device,
/// so the run is quarantined until the claim is released instead. Returns
/// whether the run was quarantined: the caller then keeps the frames and the
/// `DmaMemory` charge.
pub(super) fn before_last_drop(buffer: &Buffer, closer: Option<usize>, seen: bool) -> bool {
    let (1, Some(owner), true) = (buffer.refs, buffer.dma, seen) else {
        return false;
    };
    if closer == Some(buffer.owner) {
        crate::dev::dma_buffer_freed(owner.device, owner.generation);
        return false;
    }
    crate::dev::dma_quarantine(owner.device, owner.generation, buffer.object_id)
}

/// Allocate `pages` zeroed frames, releasing what was allocated on failure.
pub(super) fn alloc_frames(pages: u64) -> Option<Vec<PhysAddr>> {
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
pub(super) fn object_of(handle: u64, required: u32) -> Result<u64, Error> {
    let entry = handles::get(handle).map_err(from_handles)?;
    if entry.kind != HandleKind::Buffer {
        return Err(Error::WrongKind);
    }
    if entry.rights & required != required {
        return Err(Error::MissingRight);
    }
    Ok(entry.object_id)
}
