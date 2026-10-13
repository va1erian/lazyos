//! `mprotect` on the page tables: change the rights of the resident pages of a
//! range (W^X flips for a JIT, `PROT_EXEC` on the heap or stack).

use super::*;

/// Apply `prot` to the present user pages of `[start, end)` in `table`.
///
/// A copy-on-write page that another address space still shares keeps its
/// frame and its COW mark: the new protection only says what this space may
/// do, and the copy is taken by the first store ([`cow_fault`]), so a child
/// that flips a forked code page RX -> RW -> RX copies it only when it writes.
/// A COW page nobody else holds any more is just private again (P6.6). Each
/// changed PTE is flushed with `invlpg`: this is the point where a page loses
/// (or gains) execute permission, and a stale TLB entry would keep the old
/// right. That flush is local to the CPU, which is enough while the kernel
/// runs on one CPU (docs/architecture/virtual-memory.md, "Executable memory"); an SMP
/// kernel needs a shootdown here. Always returns true; the `bool` is kept for
/// callers that treat a refusal as `ENOMEM`.
pub fn protect_range(table: PhysAddr, start: u64, end: u64, prot: vma::Prot) -> bool {
    // Pages of a `MAP_SHARED` file mapping gain write access through the
    // write-protection fault (which dirties them), never here.
    let shared_files: alloc::vec::Vec<_> = vma::find_range(table, start, end)
        .into_iter()
        .filter(|piece| piece.kind == vma::Kind::FileShared)
        .collect();
    let mut va = start & !(FRAME_SIZE - 1);
    while va < end {
        // Safety: `table` is a live address space and we own its entries.
        if let Some(entry) = unsafe { leaf_entry(table, va) } {
            // Safety: `entry` was just returned as a present leaf in this table.
            let old = unsafe { *entry };
            if old & PTE_USER != 0 {
                let mut new = old;
                if old & COW_BIT != 0 && super::frame_refcount(PhysAddr::new(old & PTE_ADDR)) == 1 {
                    new &= !COW_BIT;
                }
                let mut applied = apply_prot(new, prot);
                if old & PTE_WRITABLE == 0 && shared_files.iter().any(|piece| piece.contains(va)) {
                    applied &= !PTE_WRITABLE;
                }
                if applied & COW_BIT != 0 {
                    // Still shared: stays read-only until the first store.
                    applied &= !PTE_WRITABLE;
                }
                // Safety: same `entry`, still valid; nothing else can have
                // unmapped it in between (single-threaded `mprotect`).
                unsafe { *entry = applied };
                x86_64::instructions::tlb::flush(VirtAddr::new(va));
            }
        }
        va += FRAME_SIZE;
    }
    true
}

/// Add the flags `prot` implies to an already present PTE value.
fn apply_prot(mut entry: u64, prot: vma::Prot) -> u64 {
    entry &= !(PTE_WRITABLE | PTE_NX);
    if prot.has_write() {
        entry |= PTE_WRITABLE;
    }
    if !prot.has_exec() {
        entry |= PTE_NX;
    }
    entry
}
