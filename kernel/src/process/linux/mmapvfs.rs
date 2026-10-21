//! `mmap` of a regular file (and `msync`): the cases of `mmap(fd)` whose
//! pages come from a file's page cache (`fs::openfile::pagecache`).
//!
//! Nothing is mapped when `mmap` returns: the VMA records the open file and
//! the offset, and `mem::filemap` fills in pages as they are touched.
//!
//! * `MAP_PRIVATE` is charged to the mapper like anonymous memory (its first
//!   store takes a private copy) and shows file changes it has not copied.
//! * `MAP_SHARED` maps the cache frames themselves: stores are seen by every
//!   mapper and by `read`, and reach the file at `msync`, `munmap`, exit,
//!   `fsync` or the periodic flush. It is charged to the mapper like
//!   `MAP_SHARED | MAP_ANONYMOUS`.
//! * a snapshot descriptor (`/proc` files, devices) has no pages to share: a
//!   private mapping copies it eagerly, a shared one is `ENODEV`.
//!
//! A mapping past the end of the file is allowed; touching a page wholly
//! past the end is a fault (`SIGSEGV`, where Linux raises `SIGBUS`), and a
//! file that shrinks under a mapping behaves the same way for new faults;
//! pages already mapped stay readable until unmapped.

use alloc::sync::Arc;

use x86_64::structures::paging::PageTableFlags;
use x86_64::{PhysAddr, VirtAddr};

use crate::fs::openfile::OpenFile;
use crate::mem::vma::{self, Kind, Prot};
use crate::quota::{self, Resource};
use crate::task::{self, Fd};

use super::errno::{err, EACCES, EBADF, EINVAL, ENODEV, ENOMEM, EOVERFLOW};
use super::mem::Mapping;
use super::PAGE;

/// What [`unmap_vmas`] took out of an address space.
pub(super) struct Gone {
    /// The removed pieces, clipped to the range.
    pub(super) vmas: alloc::vec::Vec<vma::Vma>,
    /// The user-memory quota they held: a piece's length, or for a
    /// reservation ([`vma::Vma::reserve`]) the pages that were resident in it
    /// (a memfd mapping's pages are the memfd's: no charge).
    pub(super) charged: u64,
}

/// The quota `vma` holds in `table`, counted before its pages are unmapped.
pub(super) fn held(table: PhysAddr, vma: &vma::Vma) -> u64 {
    if matches!(vma.kind, Kind::Shared | Kind::FileImage) {
        0
    } else if vma.reserve {
        crate::mem::resident_pages(table, vma.start, vma.end) * PAGE
    } else {
        vma.len()
    }
}

/// Take `[start, end)` out of the VMAs and the page tables, and let go of
/// the file pages its file mappings held. Returns the removed pieces and what
/// they were charged. A range with no VMA is left alone unless `always`
/// (`MAP_FIXED` clears pages the VMAs do not know).
pub(super) fn unmap_vmas(table: PhysAddr, start: u64, end: u64, always: bool) -> Gone {
    let pieces = vma::remove_files(table, start, end);
    let charged = pieces.iter().map(|piece| held(table, &piece.vma)).sum();
    if always || !pieces.is_empty() {
        crate::mem::unmap_range(table, start, end);
    }
    let vmas = pieces.iter().map(|piece| piece.vma).collect();
    crate::mem::filemap::released(pieces, true);
    Gone { vmas, charged }
}

const MS_ASYNC: u64 = 1;
const MS_INVALIDATE: u64 = 2;
const MS_SYNC: u64 = 4;

/// `mmap(fd, offset)` of a descriptor that is not a memfd.
pub(super) fn map_fd(spec: &Mapping, fd: u64, offset: u64, shared: bool) -> u64 {
    match task::fd_clone(fd as usize) {
        Some(Fd::Vfs { ref file }) => map_file(spec, Arc::clone(file), offset, shared),
        Some(Fd::File { .. }) if !shared => map_snapshot(spec, fd, offset),
        None => err(EBADF),
        Some(_) => err(ENODEV),
    }
}

/// Check what every file mapping needs of `offset` and `len`.
fn check_range(spec: &Mapping, offset: u64) -> Result<(), u64> {
    if offset & (PAGE - 1) != 0 || (offset as i64) < 0 {
        return Err(err(EINVAL));
    }
    let len = spec.end - spec.base;
    if offset.checked_add(len).is_none_or(|end| (end as i64) < 0) {
        return Err(err(EOVERFLOW));
    }
    Ok(())
}

/// A file mapping: a VMA that names `file`, demand-faulted.
fn map_file(spec: &Mapping, file: Arc<OpenFile>, offset: u64, shared: bool) -> u64 {
    if let Err(code) = check_range(spec, offset) {
        return code;
    }
    // Linux: the file must be open for reading, and a shared writable
    // mapping needs it open for writing and not append-only.
    if !file.readable() {
        return err(EACCES);
    }
    if shared && spec.prot.has_write() && (!file.writable() || file.append()) {
        return err(EACCES);
    }
    let len = spec.end - spec.base;
    if quota::charge_for_slot(task::current(), Resource::UserMemory, len).is_err() {
        return err(ENOMEM);
    }
    let table = spec.table;
    let mut replaced = 0u64;
    if spec.fixed {
        replaced = unmap_vmas(table, spec.base, spec.end, true).charged;
    }
    let kind = if shared {
        Kind::FileShared
    } else {
        Kind::FilePrivate
    };
    vma::insert_file(table, spec.base, spec.end, spec.prot, kind, file, offset);
    if !spec.fixed {
        task::set_mmap_next(spec.end);
    }
    if replaced > 0 {
        quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
    }
    spec.base
}

/// A private mapping of a snapshot descriptor: its bytes copied into
/// anonymous pages now (the snapshot is a heap copy with no pages to share).
fn map_snapshot(spec: &Mapping, fd: u64, offset: u64) -> u64 {
    if let Err(code) = check_range(spec, offset) {
        return code;
    }
    let len = spec.end - spec.base;
    if len / PAGE > crate::mem::frame_stats().free as u64 {
        return err(ENOMEM);
    }
    if quota::charge_for_slot(task::current(), Resource::UserMemory, len).is_err() {
        return err(ENOMEM);
    }
    let table = spec.table;
    let mut replaced = 0u64;
    if spec.fixed {
        replaced = unmap_vmas(table, spec.base, spec.end, true).charged;
    }
    let flags = crate::mem::prot_flags(spec.prot);
    let mut va = spec.base;
    while va < spec.end {
        if !fill_snapshot_page(table, fd, va, offset + (va - spec.base), flags) {
            crate::mem::unmap_range(table, spec.base, va);
            quota::release_for_slot(task::current(), Resource::UserMemory, len + replaced);
            return err(ENOMEM);
        }
        va += PAGE;
    }
    vma::insert(table, spec.base, spec.end, spec.prot, Kind::Anon);
    if !spec.fixed {
        task::set_mmap_next(spec.end);
    }
    if replaced > 0 {
        quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
    }
    spec.base
}

/// Map one zeroed page at `va` holding the snapshot's bytes at `offset`.
fn fill_snapshot_page(
    table: PhysAddr,
    fd: u64,
    va: u64,
    offset: u64,
    flags: PageTableFlags,
) -> bool {
    let Some(frame) = crate::mem::alloc_zeroed_frame() else {
        return false;
    };
    if let Some(bytes) = task::fd_peek_at(fd as usize, offset as usize, PAGE as usize) {
        crate::fs::openfile::pagecache::copy_to_frame(frame.as_u64(), 0, &bytes);
    }
    if crate::mem::map_page_in(table, VirtAddr::new(va), frame, flags) {
        true
    } else {
        crate::mem::free_frame(frame);
        false
    }
}

/// `mprotect` on a shared file mapping gains write access: the open file
/// must allow it (`EACCES` otherwise). The pages themselves become writable,
/// and dirty, through their write-protection fault.
pub(super) fn protect_shared(table: PhysAddr, start: u64, end: u64, prot: Prot) -> Result<(), u64> {
    if !prot.has_write() {
        return Ok(());
    }
    for piece in vma::find_range(table, start, end) {
        if piece.kind != Kind::FileShared || piece.prot.has_write() {
            continue;
        }
        match vma::file_of(table, &piece) {
            Some(file) if file.writable() && !file.append() => {}
            _ => return Err(err(EACCES)),
        }
    }
    Ok(())
}

/// `msync(addr, len, flags)`: write the dirty pages of shared file mappings
/// in the range to their files (and, for `MS_SYNC`, the files to the disk).
pub(super) fn sys_msync(addr: u64, len: u64, flags: u64) -> u64 {
    let valid = MS_ASYNC | MS_INVALIDATE | MS_SYNC;
    if addr & (PAGE - 1) != 0 || flags & !valid != 0 {
        return err(EINVAL);
    }
    if flags & MS_ASYNC != 0 && flags & MS_SYNC != 0 {
        return err(EINVAL);
    }
    let Some(end) = addr
        .checked_add(len)
        .and_then(|end| end.checked_add(PAGE - 1))
    else {
        return err(ENOMEM);
    };
    let end = end & !(PAGE - 1);
    let table = crate::mem::kernel_table();
    let pieces = vma::find_range(table, addr, end);
    // The whole range must be mapped.
    let mut covered = addr;
    let mut sorted = pieces.clone();
    sorted.sort_unstable_by_key(|piece| piece.start);
    for piece in &sorted {
        if piece.start > covered {
            break;
        }
        covered = covered.max(piece.end);
    }
    if covered < end {
        return err(ENOMEM);
    }
    let mut failed = false;
    for piece in pieces.iter().filter(|piece| piece.kind == Kind::FileShared) {
        let Some(file) = vma::file_of(table, piece) else {
            continue;
        };
        let first = piece.file_off / PAGE;
        let last = first + (piece.end - piece.start) / PAGE - 1;
        let done =
            file.writeback(first, last).is_ok() && (flags & MS_SYNC == 0 || file.flush().is_ok());
        failed |= !done;
    }
    if failed {
        return err(super::errno::EIO);
    }
    0
}
