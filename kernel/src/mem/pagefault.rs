//! Resolving page faults on user pages: the copy a write to a shared
//! copy-on-write page takes, and the zeroed page a first touch of demand-zero
//! memory gets.
//!
//! Each resolver says *why* it did not resolve a fault ([`Resolve`]): a page
//! the address space has no right to is the program's bug (`SIGSEGV`), but a
//! machine with no frame left to give is not (`oom_kill`, `SIGKILL`, as on
//! Linux). The `bool` forms ([`cow_fault`], [`demand_fault`]) exist for the kernel
//! suite, which asks only whether a page got mapped.

use super::*;

/// What a fault resolver did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolve {
    /// The page is mapped: resume the faulting instruction.
    Handled,
    /// Not a fault this path resolves (an unmapped address, a forbidden
    /// access): the caller raises `SIGSEGV`.
    Refused,
    /// The access is legitimate but the frame allocator has nothing to give.
    OutOfFrames,
}

/// Resolve a write fault on a COW page: copy the frame and map it writable.
/// Returns true if the fault was handled (caller should resume).
#[cfg(lazyos_tests)]
pub fn cow_fault(table: PhysAddr, va: u64) -> bool {
    cow_resolve(table, va) == Resolve::Handled
}

/// [`cow_fault`] that tells "not mine" from "no frame to copy into".
pub fn cow_resolve(table: PhysAddr, va: u64) -> Resolve {
    // Safety: we walk the given PML4, whose entries we own.
    let entry = unsafe { leaf_entry(table, va & !(FRAME_SIZE - 1)) };
    let Some(entry) = entry else {
        return Resolve::Refused;
    };
    // Safety: `entry` was just returned by `leaf_entry` as a present leaf in
    // this same table.
    let old = unsafe { *entry };
    if old & pte::SHARED != 0 && old & PTE_USER != 0 && old & PTE_WRITABLE == 0 {
        // A shared file page maps read-only until its first store.
        return if super::filemap::shared_write_fault(table, va, entry) {
            Resolve::Handled
        } else {
            Resolve::Refused
        };
    }
    if old & PTE_USER == 0 || old & COW_BIT == 0 {
        return Resolve::Refused;
    }
    // COW only defers the copy of a page the address space may write. A fork
    // marks every leaf COW, read-only ones (`PROT_READ`, `PROT_READ|EXEC`: a
    // JIT's code cache) included, and a private memfd view is mapped COW
    // whatever its protection; a store to one of those is an access error
    // (`SIGSEGV`, `SEGV_ACCERR`), not a request for a private copy. A page
    // with no VMA (a loader's or test's own mapping) keeps the old rule.
    if vma::forbids_write(table, va) {
        return Resolve::Refused;
    }
    let shared = PhysAddr::new(old & PTE_ADDR);
    // The other sharers already took their copies (or exited): this mapping
    // is the frame's only reference, so it becomes writable in place, with
    // no frame allocated and nothing copied (P6.6).
    if super::frame_refcount(shared) == 1 {
        // Safety: `entry` is the same present leaf read above; nothing else
        // can have unmapped it in between (single-threaded fault handling).
        unsafe {
            *entry = (old & !COW_BIT) | PTE_WRITABLE;
            make_walk_writable(table, va);
        }
        // Local on purpose: the entry only loosens (mem/tlb.rs).
        x86_64::instructions::tlb::flush(VirtAddr::new(va));
        return Resolve::Handled;
    }
    // Every byte of the new frame is overwritten by the copy: no zeroing.
    let Some(frame) = super::alloc_frame() else {
        return Resolve::OutOfFrames;
    };
    copy_frame(shared, frame);
    // Safety: `entry` is the same present leaf read above; nothing else can
    // have unmapped it in between (single-threaded fault handling).
    unsafe {
        *entry = frame.as_u64() | ((old & !PTE_ADDR) & !COW_BIT) | PTE_WRITABLE;
        make_walk_writable(table, va);
    }
    // Every CPU stops reading the shared frame through this mapping (a
    // sibling thread's stale read-only entry would keep doing so, unfaulted,
    // after its other owners write or free it) before our reference goes.
    tlb::shootdown(va, 1);
    // The page now lives privately here: release our reference to the shared
    // frame (which frees it if this was the last user).
    free_frame(PhysAddr::new(old & PTE_ADDR));
    Resolve::Handled
}

/// Resolve a not-present page fault by materializing a zeroed page for an
/// `Anon`/`Heap`/`Stack` VMA. Returns true if the fault was handled.
///
/// Only access the VMA permits is granted: a write fault in a read-only range
/// (or any access to `PROT_NONE`) stays unresolved and falls through to the
/// fatal path, where a future signal would be delivered. A file mapping is
/// filled from the page cache by [`super::filemap`]; a stack is mapped eagerly
/// only where the loader wrote its start frame, the rest of its reservation
/// fills on first touch.
#[cfg(lazyos_tests)]
pub fn demand_fault(table: PhysAddr, va: u64, error: PageFaultErrorCode) -> bool {
    demand_resolve(table, va, error) == Resolve::Handled
}

/// [`demand_fault`] that tells "not mine" from "no frame to back it".
pub fn demand_resolve(table: PhysAddr, va: u64, error: PageFaultErrorCode) -> Resolve {
    if error.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
        return Resolve::Refused; // present but forbidden: not a missing page
    }
    let Some(vma) = vma::find(table, va) else {
        return Resolve::Refused;
    };
    if vma.kind.is_file_mapping() {
        // A file page is read from the page cache (it may park the task); a
        // file that cannot be read is the program's `SIGSEGV`, a machine with
        // no frame for the page is not.
        return super::filemap::fault(table, va, error, vma);
    }
    if !matches!(
        vma.kind,
        vma::Kind::Anon | vma::Kind::Heap | vma::Kind::Stack
    ) {
        return Resolve::Refused;
    }
    // x86 has no write-only or execute-only page, so (as on Linux) any right
    // implies a readable page; only `PROT_NONE` has nothing to materialize.
    if vma.prot.0 & (vma::Prot::READ.0 | vma::Prot::WRITE.0 | vma::Prot::EXEC.0) == 0 {
        return Resolve::Refused;
    }
    let Some(frame) = alloc_zeroed_frame() else {
        return Resolve::OutOfFrames;
    };
    // A reservation's quota charge is its resident pages (and the page tables
    // they need): this one starts to count now. The fault cannot return
    // `ENOMEM`, so a uid at its quota is answered like a machine with no frame
    // left (`oom_kill`, or `ENOMEM` from the syscall that touched the page):
    // the page stays unmapped. The frame comes first so that the out-of-frames
    // path has nothing to undo (nothing there may allocate).
    let charged = if vma.reserve {
        let bytes = FRAME_SIZE * (1 + missing_tables(table, va));
        match charge_reserved(table, bytes) {
            Some(uid) => Some((uid, bytes)),
            None => {
                free_frame(frame);
                return Resolve::OutOfFrames;
            }
        }
    } else {
        None
    };
    let page = VirtAddr::new(va & !(FRAME_SIZE - 1));
    match try_map_page_in(table, page, frame, prot_flags(vma.prot)) {
        Ok(()) => Resolve::Handled,
        Err(error) => {
            free_frame(frame);
            if let Some((uid, bytes)) = charged {
                crate::quota::release_space(uid, table.as_u64(), bytes);
            }
            // A page-table frame the allocator could not give is the same
            // shortage as a data frame; anything else is a bad request.
            if matches!(error, MapToError::FrameAllocationFailed) {
                return Resolve::OutOfFrames;
            }
            crate::serial_println!("demand fault at {va:#x} failed: {error:?}");
            Resolve::Refused
        }
    }
}

/// Charge `bytes` (a page and the table frames it needs) to the user-memory
/// quota of the uid running the fault, recorded against `table` so that the
/// space's teardown gives them back. The uid, or `None` when it is at its
/// quota (the denial is counted in its ledger; the process dies with the
/// report of `mem::oom`). Allocates nothing that can fail the fault.
fn charge_reserved(table: PhysAddr, bytes: u64) -> Option<u32> {
    let uid = crate::ipc::credentials::of(crate::task::current()).uid;
    crate::quota::charge_space(uid, table.as_u64(), bytes)
        .ok()
        .map(|()| uid)
}
