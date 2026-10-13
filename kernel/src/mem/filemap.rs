//! File-backed mappings: the page-fault side of `mmap(fd)` and what happens
//! to a file's pages when a mapping goes away (`fs::openfile::pagecache`).
//!
//! A file mapping is a VMA that names its open file and an offset
//! ([`vma::Vma::file`]); no page is mapped until it is touched.
//!
//! * `MAP_SHARED`: the fault maps the file's cache frame, with the VMA's
//!   protection and `pte::SHARED`, one frame reference per mapping. `fork`
//!   copies the entry as it is, so every mapper (and the cache, which `read`
//!   consults for dirty pages) sees the same bytes.
//! * `MAP_PRIVATE`: a read fault maps the cache frame read-only and
//!   copy-on-write (the state `fork` leaves a page in), so the first store
//!   ([`super::cow_fault`]) takes a private copy and the file never changes.
//!   A write to a page not yet mapped skips the shared step and builds the
//!   private copy straight away.
//!
//! A page past the end of the file is not mapped (the access ends as a
//! `SIGSEGV`; Linux sends `SIGBUS`).

use alloc::vec::Vec;
use x86_64::structures::idt::PageFaultErrorCode;
use x86_64::structures::paging::PageTableFlags;
use x86_64::{PhysAddr, VirtAddr};

use super::{alloc_frame, free_frame, leaf_entry, map_page_in, prot_flags, vma, FRAME_SIZE};

/// `pte::SHARED` as the `x86_64` crate names the bit (52).
const SHARED_BIT: PageTableFlags = PageTableFlags::BIT_52;
/// The copy-on-write software bit (9).
const COW_FLAG: PageTableFlags = PageTableFlags::BIT_9;

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
    let (frame, flags) = match vma.kind {
        vma::Kind::FileShared => {
            let Ok(frame) = file.page_frame(index) else {
                return false;
            };
            if vma.prot.has_write() {
                // Before the page is mapped writable, so a store cannot
                // precede the mark that makes `read` look at the frame.
                file.mark_dirty(index, index);
            }
            (frame, prot_flags(vma.prot) | SHARED_BIT)
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
    // Reading the file may have slept: another thread can have mapped the
    // page, or changed or removed the mapping. Whatever the answer, the
    // faulting access is simply tried again.
    let unchanged = vma::find(table, va)
        .is_some_and(|now| now.kind == vma.kind && now.file == vma.file && now.prot == vma.prot);
    // SAFETY: `table` is the live address space this fault happened in.
    let present = unsafe { leaf_entry(table, page) }.is_some();
    if !unchanged || present {
        free_frame(frame);
        return true;
    }
    if !map_page_in(table, VirtAddr::new(page), frame, flags) {
        free_frame(frame);
        return false;
    }
    true
}

/// Pieces of file mappings were unmapped (their pages are gone from the page
/// tables): write back what shared stores changed and let go of the file's
/// pages that nothing maps any more.
pub fn released(pieces: Vec<vma::Unmapped>) {
    for piece in pieces {
        let Some(file) = piece.file else {
            continue;
        };
        let first = piece.vma.file_off / FRAME_SIZE;
        let pages = (piece.vma.end - piece.vma.start) / FRAME_SIZE;
        if pages > 0 {
            file.release_pages(first, first + pages - 1);
        }
    }
}

/// An address space is being torn down: [`released`] for every file mapping
/// it still has. Called after its page tables were freed (so the frames'
/// references are gone) and before its VMA list is forgotten.
pub(super) fn release_space(table: PhysAddr) {
    released(vma::file_mappings(table));
}
