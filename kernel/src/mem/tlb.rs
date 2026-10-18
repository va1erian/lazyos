//! TLB shootdown (docs/smp-plan.md S8.2): the one place a page mapping is
//! invalidated everywhere it may be cached.
//!
//! A local `invlpg` (and a CR3 write) only cleans *this CPU's* TLB. The
//! moment more than one CPU exists, a mapping torn down here can survive in
//! another CPU's TLB — and a stale *writable* entry over a freed frame is a
//! use-after-free that faults no one. So every unmap or write-protect of a
//! page table a running task might be using goes through [`shootdown`]:
//! local flush, then the cross-CPU half — the mailbox + `SHOOTDOWN_VECTOR`
//! doorbell (`arch::smp`), each target invalidating in its handler (which
//! obeys the window-safe contract: no locks, no allocation, no task switch).
//!
//! The callers were derived mechanically (`grep -rn "tlb::flush" kernel/src`
//! plus the flush-by-CR3-write sites: `mem/cow.rs` fork flushes that way) —
//! see `docs/architecture/smp-audit.md`, S8.2's shootdown row. The fault
//! paths (`pagefault.rs` COW/demand resolve) deliberately do NOT shoot
//! down: a resolve only tightens or installs an entry, so a remote stale
//! entry is safe.

use x86_64::VirtAddr;

/// Invalidate `[va, va + pages * 4096)` on every CPU that may hold it:
/// locally first (this CPU is certainly one of them), then the other online
/// CPUs. With one CPU online this is exactly the local flush the kernel did
/// before SMP work began.
pub fn shootdown(va: u64, pages: u64) {
    for page in 0..pages {
        x86_64::instructions::tlb::flush(VirtAddr::new(va + page * 4096));
    }
    if crate::arch::smp::online_cpus() > 1 {
        crate::arch::smp::shootdown_others(va, pages);
    }
}

/// Invalidate everything (the `flush_all` case: a PAT change, a table
/// switch that must be observed everywhere). The local `flush_all`, then
/// the others' — each target runs its own `flush_all` in the handler.
pub fn shootdown_all() {
    x86_64::instructions::tlb::flush_all();
    if crate::arch::smp::online_cpus() > 1 {
        // The shootdown handler's "no pages" case is a full flush: address 0.
        crate::arch::smp::shootdown_others(0, 0);
    }
}
