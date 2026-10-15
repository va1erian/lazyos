//! File-backed mappings: the page-fault side of `mmap(fd)` and what happens
//! to a file's pages when a mapping goes away (`fs::openfile::pagecache`).
//!
//! A file mapping is a VMA that names its open file and an offset
//! ([`vma::Vma::file`]); no page is mapped until it is touched.
//!
//! * `MAP_SHARED`: the fault maps the file's cache frame with `pte::SHARED`,
//!   one frame reference per mapping; `fork` copies the entry as it is, so
//!   every mapper (and the cache, which `read` consults for dirty pages)
//!   sees the same bytes. A page is mapped **read-only** by a read fault and
//!   writable only by a store: the store's write-protection fault
//!   ([`shared_write_fault`]) marks the page dirty, and a write-back
//!   write-protects every mapping of the page again ([`write_protect`]), so a
//!   page that is only read is never written back and a written one is
//!   written once per change.
//! * `MAP_PRIVATE`: a read fault maps the cache frame read-only and
//!   copy-on-write (the state `fork` leaves a page in), so the first store
//!   ([`super::cow_fault`]) takes a private copy and the file never changes.
//!   A write to a page not yet mapped skips the shared step and builds the
//!   private copy straight away.
//!
//! A page past the end of the file is not mapped (the access ends as a
//! `SIGSEGV`; Linux sends `SIGBUS`).
//!
//! Reading the file can park the faulting task (disk I/O), and while it is
//! parked a sibling thread can change the address space. [`fault_epoch`]
//! counts the faults that may have parked; the user-memory validators
//! (`ipc::syscalls::usermem`) read it around a validation and redo the
//! validation when it moved, so a range they checked is still there when the
//! kernel writes to it.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::structures::idt::PageFaultErrorCode;
use x86_64::structures::paging::PageTableFlags;
use x86_64::{PhysAddr, VirtAddr};

use super::{
    alloc_frame, free_frame, kernel_table, leaf_entry, make_walk_writable, map_page_in, prot_flags,
    pte, vma, FRAME_SIZE,
};

/// `pte::SHARED` as the `x86_64` crate names the bit (52).
const SHARED_BIT: PageTableFlags = PageTableFlags::BIT_52;
/// The copy-on-write software bit (9).
const COW_FLAG: PageTableFlags = PageTableFlags::BIT_9;

/// Faults that read a file (and so may have parked), counted.
static FAULT_EPOCH: AtomicU64 = AtomicU64::new(0);

/// How many faults so far may have parked the faulting task. A validation
/// that sees this move between its start and its end must be repeated.
pub fn fault_epoch() -> u64 {
    FAULT_EPOCH.load(Ordering::Acquire)
}

/// Test hook: run right after a fault read the file, where a real fault may
/// have parked, so a test can change the address space "meanwhile".
#[cfg(lazyos_tests)]
pub static AFTER_FILE_READ: spin::Mutex<Option<fn()>> = spin::Mutex::new(None);

fn parked_here() {
    #[cfg(lazyos_tests)]
    {
        let hook = *AFTER_FILE_READ.lock();
        if let Some(hook) = hook {
            hook();
        }
    }
}

/// Resolve a not-present fault at `va` inside the file mapping `vma`. True
/// when the access should be retried (the page is mapped, or the mapping
/// changed while the file was read and the fault re-evaluates).
pub(super) fn fault(table: PhysAddr, va: u64, error: PageFaultErrorCode, vma: vma::Vma) -> bool {
    // As for anonymous memory, only `PROT_NONE` has nothing to materialise.
    if vma.prot.0 & 7 == 0 {
        return false;
    }
    let page = va & !(FRAME_SIZE - 1);
    let Some(file) = vma::file_of(table, &vma) else {
        return false;
    };
    let index = vma.offset_at(page) / FRAME_SIZE;
    let store = error.contains(PageFaultErrorCode::CAUSED_BY_WRITE);
    FAULT_EPOCH.fetch_add(1, Ordering::AcqRel);
    let (frame, flags) = match vma.kind {
        vma::Kind::FileShared => {
            let Ok(frame) = file.page_frame(index) else {
                return false;
            };
            let mut flags = prot_flags(vma.prot) | SHARED_BIT;
            if store && vma.prot.has_write() {
                // Dirty before the page is writable, so a store cannot
                // precede the mark that makes `read` look at the frame.
                file.mark_dirty(index, index);
            } else {
                // Read-only until the first store (`shared_write_fault`).
                flags -= PageTableFlags::WRITABLE;
            }
            (frame, flags)
        }
        vma::Kind::FilePrivate if store && vma.prot.has_write() => {
            let Some(frame) = alloc_frame() else {
                return false;
            };
            if file.copy_page(index, frame).is_err() {
                free_frame(frame);
                return false;
            }
            (frame, prot_flags(vma.prot))
        }
        vma::Kind::FilePrivate => {
            let Ok(frame) = file.page_frame(index) else {
                return false;
            };
            let flags = (prot_flags(vma.prot) - PageTableFlags::WRITABLE) | COW_FLAG;
            (frame, flags)
        }
        _ => return false,
    };
    parked_here();
    // Reading the file may have parked: another thread can have mapped the
    // page, or changed or removed the mapping. Whatever the answer, the
    // faulting access is simply tried again.
    let unchanged = vma::find(table, va).is_some_and(|now| {
        now.kind == vma.kind
            && now.file == vma.file
            && now.prot == vma.prot
            && now.offset_at(page) == vma.offset_at(page)
    });
    // SAFETY: `table` is the live address space this fault happened in.
    let present = unsafe { leaf_entry(table, page) }.is_some();
    if !unchanged || present {
        free_frame(frame);
        // The page may have stayed in the cache with nothing mapping it.
        file.trim_page(index);
        return true;
    }
    if !map_page_in(table, VirtAddr::new(page), frame, flags) {
        free_frame(frame);
        file.trim_page(index);
        return false;
    }
    true
}

/// A store to a present, read-only page of a shared mapping: if the VMA
/// allows writing and maps a file, mark the page dirty and make this entry
/// writable. `entry` is the page's leaf in `table`.
pub(super) fn shared_write_fault(table: PhysAddr, va: u64, entry: *mut u64) -> bool {
    let Some(vma) = vma::find(table, va) else {
        return false;
    };
    if vma.kind != vma::Kind::FileShared || !vma.prot.has_write() {
        return false;
    }
    let Some(file) = vma::file_of(table, &vma) else {
        return false;
    };
    let index = vma.offset_at(va & !(FRAME_SIZE - 1)) / FRAME_SIZE;
    file.mark_dirty(index, index);
    // SAFETY: `entry` is the present leaf `cow_fault` just read in `table`;
    // nothing can have unmapped it since (no parking between).
    unsafe {
        *entry |= pte::WRITABLE;
        make_walk_writable(table, va);
    }
    x86_64::instructions::tlb::flush(VirtAddr::new(va));
    true
}

/// Write-protect, in every address space that maps `file` `MAP_SHARED`, the
/// pages `indices` of it: called by a write-back before it writes them, so
/// the next store faults and dirties the page again.
pub fn write_protect(file: usize, indices: &[u64]) {
    let current = kernel_table();
    for (table, vma, mapped) in vma::all_file_mappings() {
        if vma.kind != vma::Kind::FileShared || mapped.inode_id() != file {
            continue;
        }
        let first = vma.file_off / FRAME_SIZE;
        let pages = (vma.end - vma.start) / FRAME_SIZE;
        for &index in indices.iter().filter(|&&i| i >= first && i < first + pages) {
            let va = vma.start + (index - first) * FRAME_SIZE;
            // SAFETY: `table` is a live address space (the registry lists
            // only those); its entries are ours to change.
            if let Some(entry) = unsafe { leaf_entry(table, va) } {
                // SAFETY: just returned as a present leaf of `table`.
                let value = unsafe { *entry };
                if value & pte::WRITABLE != 0 && value & pte::SHARED != 0 {
                    // SAFETY: as above.
                    unsafe { *entry = value & !pte::WRITABLE };
                    if table == current {
                        x86_64::instructions::tlb::flush(VirtAddr::new(va));
                    }
                }
            }
        }
    }
}

/// The file of identity `file` shrank: unmap, in every address space, the
/// pages of it from page index `keep` on (shared and private, copies
/// included), so a store cannot go on to a frame the file no longer owns and
/// the next touch finds the page past the end.
pub fn unmap_from(file: usize, keep: u64) {
    for (table, vma, mapped) in vma::all_file_mappings() {
        if mapped.inode_id() != file {
            continue;
        }
        let first = vma.file_off / FRAME_SIZE;
        let pages = (vma.end - vma.start) / FRAME_SIZE;
        let from = keep.max(first);
        if from >= first + pages {
            continue;
        }
        let start = vma.start + (from - first) * FRAME_SIZE;
        // `unmap_range` flushes the TLB entry of each page it clears.
        super::unmap_range(table, start, vma.end);
    }
}

/// Pieces of file mappings were unmapped (their pages are gone from the page
/// tables): write back what shared stores changed and let go of the file's
/// pages that nothing maps any more. `wait` false does not park on a busy
/// volume (address-space teardown).
pub fn released(pieces: Vec<vma::Unmapped>, wait: bool) {
    for piece in pieces {
        let Some(file) = piece.file else {
            continue;
        };
        let first = piece.vma.file_off / FRAME_SIZE;
        let pages = (piece.vma.end - piece.vma.start) / FRAME_SIZE;
        if pages > 0 {
            file.release_pages(first, first + pages - 1, wait);
        }
    }
}

/// Write back the dirty pages of `table`'s shared file mappings now, waiting
/// for the volume: an exiting task does this in its own context, so the
/// teardown that follows (possibly on another task) has nothing left to wait
/// for.
pub fn flush_table(table: PhysAddr) {
    for piece in vma::file_mappings(table) {
        let (Some(file), true) = (piece.file, piece.vma.kind == vma::Kind::FileShared) else {
            continue;
        };
        let first = piece.vma.file_off / FRAME_SIZE;
        let pages = (piece.vma.end - piece.vma.start) / FRAME_SIZE;
        if pages > 0 {
            if let Err(error) = file.writeback(first, first + pages - 1) {
                crate::serial_println!("mmap: writeback of {} failed: {error:?}", file.path());
            }
        }
    }
}
