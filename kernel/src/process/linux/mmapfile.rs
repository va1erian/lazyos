//! The `mmap` cases that are not anonymous private memory: `MAP_SHARED |
//! MAP_ANONYMOUS` and a memfd mapped `MAP_SHARED` or `MAP_PRIVATE`.
//!
//! These ranges are mapped eagerly. A shared page has one frame that every
//! mapping must name, and demand-faulting would give two processes two
//! different zero pages; so the frames exist, and are in the page tables,
//! when `mmap` returns:
//!
//! * a **shared** mapping puts each frame in with the `pte::SHARED` bit and
//!   the VMA's own protection, one frame reference per page. `fork` copies the
//!   entry as it is (`mem::cow`), so a parent and its children, or two
//!   processes holding the same memfd, see each other's stores. A memfd's
//!   pages are charged to the memfd, once; `MAP_SHARED | MAP_ANONYMOUS`
//!   pages are charged to the mapper like any anonymous memory.
//! * a **private** memfd mapping maps the memfd's frames read-only and
//!   copy-on-write (the state `fork` leaves a page in), so the first store
//!   takes a private copy and the file never changes. It is charged and
//!   released like anonymous memory.
//!
//! A `PROT_NONE` mapping of either kind is refused (`EINVAL`): its pages
//! cannot be mapped without being readable, and there is no fault path to
//! fill them in when `mprotect` opens them later. A regular file is mapped
//! lazily from its page cache instead ([`super::mmapvfs`]).

use x86_64::structures::paging::PageTableFlags;
use x86_64::{PhysAddr, VirtAddr};

use crate::mem::vma::Kind;
use crate::quota::{self, Resource};
use crate::task::{self, Fd};

use super::errno::{err, EBADF, EINVAL, ENOMEM, EOVERFLOW};
use super::mem::Mapping;
use super::PAGE;

/// Software page-table bits as the `x86_64` crate names them: 52 is
/// `pte::SHARED`, 9 the copy-on-write bit.
const SHARED_BIT: PageTableFlags = PageTableFlags::BIT_52;
const COW_BIT: PageTableFlags = PageTableFlags::BIT_9;

/// Whether the frame allocator could back `len` more bytes at all: an
/// eagerly mapped range larger than free memory is refused up front instead of
/// being filled until the machine is empty and rolled back.
fn fits_in_memory(len: u64) -> bool {
    len / PAGE <= crate::mem::frame_stats().free as u64
}

/// `mmap(MAP_SHARED | MAP_ANONYMOUS)`: zeroed pages every `fork` child keeps
/// sharing.
pub(super) fn map_shared_anon(spec: &Mapping) -> u64 {
    if spec.prot.0 == 0 {
        return err(EINVAL);
    }
    let len = spec.end - spec.base;
    if !fits_in_memory(len) {
        return err(ENOMEM);
    }
    if quota::charge_for_slot(task::current(), Resource::UserMemory, len).is_err() {
        return err(ENOMEM);
    }
    let flags = crate::mem::prot_flags(spec.prot) | SHARED_BIT;
    let result = install(spec, Kind::SharedAnon, flags, |_| {
        crate::mem::alloc_zeroed_frame().ok_or(err(ENOMEM))
    });
    if let Err(code) = result {
        quota::release_for_slot(task::current(), Resource::UserMemory, len);
        return code;
    }
    spec.base
}

/// `mmap(fd, offset)` of a memfd: shared pages, or a private copy-on-write
/// view of them.
pub(super) fn map_memfd(spec: &Mapping, fd: u64, offset: u64, shared: bool) -> u64 {
    let opened = task::fd_clone(fd as usize);
    let mem = match &opened {
        Some(Fd::Memfd { mem }) => mem,
        None => return err(EBADF),
        // A regular file maps from its page cache (`mmapvfs`).
        Some(_) => return super::mmapvfs::map_fd(spec, fd, offset, shared),
    };
    if offset & (PAGE - 1) != 0 || (offset as i64) < 0 {
        return err(EINVAL);
    }
    let len = spec.end - spec.base;
    if offset.checked_add(len).is_none_or(|end| (end as i64) < 0) {
        return err(EOVERFLOW);
    }
    if spec.prot.0 == 0 {
        return err(EINVAL);
    }
    if !fits_in_memory(len) {
        return err(ENOMEM);
    }
    let first = offset / PAGE;
    let frame_for = |index: u64| {
        mem.page_for_mapping(first + index)
            .map_err(super::memfd::mem_err)
    };
    if shared {
        let flags = crate::mem::prot_flags(spec.prot) | SHARED_BIT;
        return match install(spec, Kind::Shared, flags, frame_for) {
            Ok(()) => spec.base,
            Err(code) => code,
        };
    }
    // Private: charged like anonymous memory, mapped read-only and COW so the
    // first store copies the page and `mprotect(PROT_WRITE)` privatises it.
    if quota::charge_for_slot(task::current(), Resource::UserMemory, len).is_err() {
        return err(ENOMEM);
    }
    let flags = (crate::mem::prot_flags(spec.prot) - PageTableFlags::WRITABLE) | COW_BIT;
    match install(spec, Kind::Anon, flags, frame_for) {
        Ok(()) => spec.base,
        Err(code) => {
            quota::release_for_slot(task::current(), Resource::UserMemory, len);
            code
        }
    }
}

/// Map every page of `spec` with the frame `frame_for(page index)` supplies
/// (one reference per page, owned by the mapping), then record the VMA. A
/// `MAP_FIXED` request replaces what was there first. On failure every page
/// mapped so far is unmapped again and the VMA is not recorded.
fn install(
    spec: &Mapping,
    kind: Kind,
    flags: PageTableFlags,
    mut frame_for: impl FnMut(u64) -> Result<PhysAddr, u64>,
) -> Result<(), u64> {
    let table = spec.table;
    let mut replaced = 0u64;
    if spec.fixed {
        replaced = super::mmapvfs::unmap_vmas(table, spec.base, spec.end, true).charged;
    }
    let mut va = spec.base;
    while va < spec.end {
        let step = frame_for((va - spec.base) / PAGE).and_then(|frame| {
            if crate::mem::map_page_in(table, VirtAddr::new(va), frame, flags) {
                Ok(())
            } else {
                crate::mem::free_frame(frame);
                Err(err(ENOMEM))
            }
        });
        if let Err(code) = step {
            crate::mem::unmap_range(table, spec.base, va);
            if replaced > 0 {
                quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
            }
            return Err(code);
        }
        va += PAGE;
    }
    crate::mem::vma::insert(table, spec.base, spec.end, spec.prot, kind);
    if !spec.fixed {
        task::set_mmap_next(spec.end);
    }
    if replaced > 0 {
        quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
    }
    Ok(())
}
