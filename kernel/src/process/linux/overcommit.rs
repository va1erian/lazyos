//! Refusing anonymous memory the machine could never back (Linux's
//! `vm.overcommit_memory = 0`, the heuristic mode).
//!
//! Private anonymous pages are demand-zero: `mmap`, `brk` and `mremap` charge
//! the user-memory quota and record a VMA, and a frame is found at the first
//! touch. A request larger than every frame the allocator owns can only end
//! one way (the touch finds nothing and the process is killed, `mem::oom`), so
//! it is refused where the program can still see it: `ENOMEM` from the call,
//! which is what a JavaScript engine's "allocation failed" path or a
//! `malloc` returning null expects.
//!
//! The rule is Linux's: one request against the machine's total, not against
//! what is free right now or what the process already holds (a sparse reservation
//! that fits must work, and free memory changes under the caller). What it does
//! not catch (many requests that each fit, or one that fits the machine but not
//! the memory left on a busy one) is the page fault's job. `PROT_NONE` ranges and `MAP_NORESERVE`
//! ones commit nothing and are never refused here, which is how a runtime
//! reserves a large address range and commits it in pieces; they are still
//! charged against the user-memory quota.

use crate::mem::vma::Prot;

use super::PAGE;

/// `mmap`'s `MAP_NORESERVE`: do not account for the pages up front.
pub(super) const MAP_NORESERVE: u64 = 0x4000;

/// Whether `bytes` of new private anonymous memory with `prot` could never be
/// backed, so the call should fail with `ENOMEM` now.
pub(super) fn hopeless(prot: Prot, bytes: u64) -> bool {
    // A mapping with no right commits no page (a fault on it is `SIGSEGV`).
    if prot.0 & 0x7 == 0 {
        return false;
    }
    bytes / PAGE > crate::mem::frame_stats().total as u64
}
