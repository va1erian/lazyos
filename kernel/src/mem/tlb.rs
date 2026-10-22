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
//! plus the flush-by-CR3-write sites: `mem/cow.rs` fork flushes that way) --
//! see `docs/architecture/smp-audit.md`, S8.2's shootdown row.
//!
//! What stays local, and why. A fault resolve that only *tightens* nothing
//! and only *installs or loosens* an entry (demand-zero, a COW page made
//! writable in place, a shared page made writable) leaves a remote CPU at
//! worst with a stale read-only entry: its next store faults, the CPU drops
//! the entry, and `arch::spurious_fault` retries the store. The COW **copy**
//! is different: the faulting space drops its reference to the old frame,
//! and a stale entry on another CPU (a sibling thread) would keep *reading*
//! that frame, with no fault, after its other owner writes or frees it. That
//! path shoots down before it releases the frame.

use x86_64::VirtAddr;

/// Above this many pages a flush is one `flush_all` instead (a page-by-page
/// `invlpg` walk loses to it, and never truncating is the rule: a stale
/// entry left behind is a use-after-free that faults no one).
pub const FLUSH_ALL_ABOVE: u64 = 64;

/// Invalidate `[va, va + pages * 4096)` on this CPU alone.
fn flush_local(va: u64, pages: u64) {
    if pages > FLUSH_ALL_ABOVE {
        x86_64::instructions::tlb::flush_all();
        return;
    }
    for page in 0..pages {
        x86_64::instructions::tlb::flush(VirtAddr::new(va + page * 4096));
    }
}

/// The other CPUs' half: nothing to do (and nothing touched) with one CPU
/// online, which is the whole of today's default boot.
fn flush_others(va: u64, pages: u64) {
    if crate::arch::smp::online_cpus() > 1 {
        crate::arch::smp::shootdown_others(va, pages);
    }
}

/// Invalidate `[va, va + pages * 4096)` on every CPU that may hold it:
/// locally first (this CPU is certainly one of them), then the other online
/// CPUs. With one CPU online this is exactly the local flush the kernel did
/// before SMP work began. `pages` must be at least 1 (0 pages is the
/// "everything" case on the wire; use [`shootdown_all`]).
pub fn shootdown(va: u64, pages: u64) {
    debug_assert!(pages > 0, "shootdown of no pages is shootdown_all");
    flush_local(va, pages);
    flush_others(va, pages);
}

/// Invalidate everything (the `flush_all` case: a PAT change, a fork that
/// downgraded the parent's leaves in place). The local `flush_all`, then
/// the others' — each target runs its own `flush_all` in the handler.
pub fn shootdown_all() {
    x86_64::instructions::tlb::flush_all();
    // The shootdown handler's "no pages" case is a full flush: address 0.
    flush_others(0, 0);
}

/// Translations changed one page at a time, flushed on this CPU as they are
/// noted (exactly the `invlpg` each site did before) and on the other CPUs
/// once, for the span covered, at [`Batch::finish`]. A loop that tears down
/// many pages costs one cross-CPU round trip instead of one per page.
///
/// The caller must `finish` before anything can observe the change as
/// final: before a frame the pages named is freed (the frame could be
/// reallocated while a remote CPU still writes through the stale entry) and
/// before the call returns to a caller that relies on the new rights.
#[must_use = "a Batch that is never finished leaves other CPUs' TLBs stale"]
pub struct Batch {
    lo: u64,
    hi: u64,
}

impl Batch {
    pub const fn new() -> Self {
        Self {
            lo: u64::MAX,
            hi: 0,
        }
    }

    /// `va`'s translation changed: drop it from this CPU's TLB now, and
    /// remember it for the others.
    pub fn note(&mut self, va: u64) {
        let page = va & !4095;
        x86_64::instructions::tlb::flush(VirtAddr::new(page));
        self.lo = self.lo.min(page);
        self.hi = self.hi.max(page + 4096);
    }

    /// Whether no page is pending for the other CPUs.
    pub fn is_clean(&self) -> bool {
        self.hi <= self.lo
    }

    /// Flush the noted span on the other CPUs; the batch is empty after.
    pub fn finish(&mut self) {
        if !self.is_clean() {
            flush_others(self.lo, (self.hi - self.lo) / 4096);
        }
        // Field by field: assigning a whole `Self` would run `drop` on the
        // old value while it still reads as pending.
        self.lo = u64::MAX;
        self.hi = 0;
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        debug_assert!(self.is_clean(), "tlb::Batch dropped with pages pending");
    }
}
