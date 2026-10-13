//! The `mmap` family: `mmap`, `munmap`, `mprotect`, `brk` and `mremap`.
//!
//! All five work the same way underneath: update the [`crate::mem::vma`]
//! table first (the source of truth for what a range means and who owns the
//! quota charge for it), then make the page tables agree, deferring frames to
//! first touch wherever demand-zero is safe.

use alloc::vec::Vec;
use x86_64::PhysAddr;

use crate::mem::vma::{Kind, Prot, Vma};
use crate::quota::{self, Resource};
use crate::task;

use super::errno::{err, EFAULT, EINVAL, ENOMEM};
use super::{BRK_LIMIT, MMAP_BASE, MMAP_LIMIT, PAGE};

const MAP_SHARED: u64 = 0x1;
const MAP_SHARED_VALIDATE: u64 = 0x3;
/// The mask of the sharing type in `mmap`'s flags (`MAP_SHARED`,
/// `MAP_PRIVATE`, `MAP_SHARED_VALIDATE`).
const MAP_TYPE: u64 = 0xf;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;

/// `mremap(2)` flags.
const MREMAP_MAYMOVE: u64 = 1;
const MREMAP_FIXED: u64 = 2;

/// Round `value` up to a multiple of `align`, or `None` if the rounding would
/// overflow `u64`. All callers feed this untrusted user lengths, so the
/// addition must be checked: the default profile panics on overflow.
fn align_up(value: u64, align: u64) -> Option<u64> {
    value
        .checked_add(align - 1)
        .map(|value| value & !(align - 1))
}

/// Where an `mmap` lands: a page-aligned `[base, end)` in address space
/// `table`, with the protection and whether `MAP_FIXED` named the place.
pub(super) struct Mapping {
    pub(super) table: PhysAddr,
    pub(super) base: u64,
    pub(super) end: u64,
    pub(super) prot: Prot,
    pub(super) fixed: bool,
}

/// The bytes of `vmas` whose quota charge `munmap` and its relatives give
/// back: a memfd mapping's pages are charged to the memfd, not the mapping.
pub(super) fn charged_len(vmas: &[crate::mem::vma::Vma]) -> u64 {
    vmas.iter()
        .filter(|vma| vma.kind != Kind::Shared)
        .map(|vma| vma.len())
        .sum()
}

/// Take `[start, end)` out of the VMAs and the page tables, and let go of
/// the file pages its file mappings held. Returns the removed pieces. A range
/// with no VMA is left alone unless `always` (`MAP_FIXED` clears pages the
/// VMAs do not know).
pub(super) fn unmap_vmas(table: PhysAddr, start: u64, end: u64, always: bool) -> Vec<Vma> {
    let pieces = crate::mem::vma::remove_files(table, start, end);
    if always || !pieces.is_empty() {
        crate::mem::unmap_range(table, start, end);
    }
    let vmas = pieces.iter().map(|piece| piece.vma).collect();
    crate::mem::filemap::released(pieces);
    vmas
}

/// `mmap(addr, len, prot, flags, fd, offset)`.
///
/// Anonymous private memory is recorded as an `Anon` VMA and populated lazily
/// (demand-zero): no frames are spent until a page is first touched, which
/// matches Linux for a buffer that is allocated and never (fully) used.
/// `MAP_FIXED` replaces whatever was mapped there before, VMAs and page
/// tables alike. Shared anonymous and memfd mappings are
/// [`super::mmapfile`]'s, a regular file's [`super::mmapvfs`].
pub(super) fn sys_mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: u64, offset: u64) -> u64 {
    let anonymous = flags & MAP_ANONYMOUS != 0;
    let sharing = flags & MAP_TYPE;
    // A file mapping must say how it is shared; an anonymous one that does
    // not is private, as it always was here.
    if !anonymous && sharing == 0 {
        return err(EINVAL);
    }
    if len == 0 {
        return err(EINVAL);
    }
    let Some(len) = align_up(len, PAGE) else {
        return err(ENOMEM);
    };
    let table = crate::mem::kernel_table();
    let base = if flags & MAP_FIXED != 0 {
        addr & !0xFFF
    } else {
        match first_free_range(table, len) {
            Some(base) => base,
            None => return err(ENOMEM),
        }
    };
    let Some(end) = base.checked_add(len) else {
        return err(ENOMEM);
    };
    if end > MMAP_LIMIT {
        return err(ENOMEM);
    }
    let prot = Prot((prot & 0x7) as u8);
    let shared = sharing == MAP_SHARED || sharing == MAP_SHARED_VALIDATE;
    let spec = Mapping {
        table,
        base,
        end,
        prot,
        fixed: flags & MAP_FIXED != 0,
    };
    match (anonymous, shared) {
        (true, true) => return super::mmapfile::map_shared_anon(&spec),
        (false, shared) => return super::mmapfile::map_memfd(&spec, fd, offset, shared),
        (true, false) => {}
    }
    // Per-uid user-memory quota (issue #103), charged before any VMA or page
    // table changes so a refusal leaves the address space untouched. The
    // closest Linux errno for "over the user's memory quota" is ENOMEM.
    if quota::charge_for_slot(task::current(), Resource::UserMemory, len).is_err() {
        return err(ENOMEM);
    }
    let mut replaced = 0u64;
    if flags & MAP_FIXED != 0 {
        replaced = charged_len(&unmap_vmas(table, base, end, true));
    }
    crate::mem::vma::insert(table, base, end, prot, Kind::Anon);
    if flags & MAP_FIXED == 0 {
        task::set_mmap_next(end);
    }
    // A fixed mapping that replaced live ranges gives their charge back.
    if replaced > 0 {
        quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
    }
    base
}

/// `munmap(addr, len)`: drop the VMAs and release any resident pages. Linux
/// accepts unmapping an unmapped range, so this always returns 0 for valid
/// arguments. COW frames only lose this address space's reference.
pub(super) fn sys_munmap(addr: u64, len: u64) -> u64 {
    if len == 0 {
        return err(EINVAL);
    }
    let start = addr & !0xFFF;
    let Some(end) = addr.checked_add(len).and_then(|end| align_up(end, PAGE)) else {
        return err(EINVAL);
    };
    let table = crate::mem::kernel_table();
    let removed = unmap_vmas(table, start, end, false);
    if !removed.is_empty() {
        // Unmapping gives the user's quota back (issue #103).
        let bytes = charged_len(&removed);
        quota::release_for_slot(task::current(), Resource::UserMemory, bytes);
    }
    0
}

/// `madvise(MADV_DONTNEED)`: anonymous and heap pages in the range are
/// released (their VMAs stay, so the next touch demand-zeroes a fresh page,
/// which is exactly the contract), stack pages are zeroed in place (they are
/// mapped eagerly), a shared mapping is left alone (its contents survive
/// `MADV_DONTNEED` on Linux too, and its pages are not ours to drop), and a
/// file segment, whose contract is "reload from the file", is refused with
/// `EINVAL`.
pub(super) fn dontneed(addr: u64, len: u64) -> u64 {
    let Some(end) = addr.checked_add(len).and_then(|end| align_up(end, PAGE)) else {
        return err(EINVAL);
    };
    let table = crate::mem::kernel_table();
    let vmas = crate::mem::vma::find_range(table, addr, end);
    if vmas.iter().any(|vma| vma.kind == Kind::File) {
        return err(EINVAL);
    }
    for vma in vmas {
        let (start, stop) = (vma.start.max(addr), vma.end.min(end));
        match vma.kind {
            Kind::Shared | Kind::SharedAnon => {}
            // Private modifications go; a shared page's contents are the
            // file's, and the next touch maps it again.
            Kind::Stack => {
                if vma.prot.has_write() {
                    let zeros = [0u8; PAGE as usize];
                    for page in (start..stop).step_by(PAGE as usize) {
                        let _ = crate::user_ptr::try_copy_to(page, &zeros);
                    }
                }
            }
            _ => {
                crate::mem::unmap_range(table, start, stop);
            }
        }
    }
    0
}

/// The `prot` bits `mprotect` accepts: `PROT_READ | PROT_WRITE | PROT_EXEC`.
const PROT_VALID: u64 = 0x7;
/// `PROT_SEM`: accepted and ignored, as on x86 Linux (it has no meaning there).
const PROT_SEM: u64 = 0x8;
/// `PROT_GROWSDOWN` / `PROT_GROWSUP`: Linux strips them (they extend the
/// range to the stack VMA's edge, which does not apply here) and refuses
/// only the two together.
const PROT_GROWSDOWN: u64 = 0x0100_0000;
const PROT_GROWSUP: u64 = 0x0200_0000;

/// `mprotect(addr, len, prot)`: update the PTE flags for resident pages and
/// the VMA for the whole range, so pages faulted in later honor the new access
/// too. Resident COW pages are privatized first (their protection is per
/// address space). A range with no VMA is a no-op: the old shim ignored it, and
/// we have no signal to deliver an ENOMEM against anyway.
pub(super) fn sys_mprotect(addr: u64, len: u64, prot: u64) -> u64 {
    // `PROT_NONE` or any mix of read, write and exec, plus the bits Linux
    // tolerates (`PROT_SEM`, one of the `GROWS` flags); anything else is
    // `EINVAL` whatever the length, so a zero-length call cannot hide one.
    if prot & (PROT_GROWSDOWN | PROT_GROWSUP) == PROT_GROWSDOWN | PROT_GROWSUP {
        return err(EINVAL);
    }
    let prot = prot & !(PROT_SEM | PROT_GROWSDOWN | PROT_GROWSUP);
    if prot & !PROT_VALID != 0 {
        return err(EINVAL);
    }
    // A zero length is a successful no-op on an aligned address, as on Linux
    // (JavaScriptCore's growable ArrayBuffer protects 0 bytes while it is empty).
    if len == 0 {
        return if addr.is_multiple_of(PAGE) {
            0
        } else {
            err(EINVAL)
        };
    }
    let start = addr & !0xFFF;
    let Some(end) = addr.checked_add(len).and_then(|end| align_up(end, PAGE)) else {
        return err(EINVAL);
    };
    let prot = Prot(prot as u8);
    let table = crate::mem::kernel_table();
    if crate::mem::vma::find_range(table, start, end).is_empty() {
        return 0;
    }
    if let Err(code) = super::mmapvfs::protect_shared(table, start, end, prot) {
        return code;
    }
    if !crate::mem::protect_range(table, start, end, prot) {
        return err(ENOMEM);
    }
    crate::mem::vma::protect(table, start, end, prot);
    0
}

/// `brk(addr)`: move the heap break. Growth records a `Heap` VMA and defers
/// the frames to first touch; shrinking unmaps what lies above the new break.
pub(super) fn sys_brk(addr: u64) -> u64 {
    let current = task::brk();
    if addr == 0 || addr < task::brk_start() {
        return current;
    }
    let Some(new) = align_up(addr, PAGE) else {
        return current;
    };
    if new > BRK_LIMIT {
        return current;
    }
    let table = crate::mem::kernel_table();
    // The break never grows over another mapping: `vma::insert` would silently
    // turn it into heap (a `MAP_FIXED` mapping may sit inside the brk range),
    // handing out live memory and letting a later shrink unmap it. Linux
    // fails the same collision by leaving the break unchanged.
    if new > current && !crate::mem::vma::find_range(table, current, new).is_empty() {
        return current;
    }
    if new > current {
        // Per-uid user-memory quota (issue #103): charge the growth before the
        // VMA exists. A refusal reports the unchanged break, which is how a
        // caller detects a failed brk.
        if quota::charge_for_slot(task::current(), Resource::UserMemory, new - current).is_err() {
            return current;
        }
        crate::mem::vma::insert(table, current, new, Prot::READ | Prot::WRITE, Kind::Heap);
    } else if new < current {
        crate::mem::vma::remove(table, new, current);
        crate::mem::unmap_range(table, new, current);
        quota::release_for_slot(task::current(), Resource::UserMemory, current - new);
    }
    task::set_brk(new);
    new
}

/// `mremap(old_address, old_size, new_size, flags, new_address)`: grow, shrink
/// or relocate an existing mapping.
///
/// The whole `[old_address, old_address + old_size)` range must be exactly one
/// VMA. Shrinking and growing a heap/anonymous mapping in place just adjust the
/// VMA (new pages stay demand-zero); anything else with `MREMAP_MAYMOVE`
/// relocates the resident PTEs to a fresh range (`MREMAP_FIXED` places it
/// exactly). Overlapping source and destination ranges are refused with
/// `-EINVAL`; Linux supports them, but nothing here needs that yet.
pub(super) fn sys_mremap(
    old_addr: u64,
    old_size: u64,
    new_size: u64,
    flags: u64,
    new_addr: u64,
) -> u64 {
    if old_addr & (PAGE - 1) != 0 || (flags & MREMAP_FIXED != 0 && new_addr & (PAGE - 1) != 0) {
        return err(EINVAL);
    }
    if old_size == 0 || new_size == 0 || flags & !(MREMAP_MAYMOVE | MREMAP_FIXED) != 0 {
        return err(EINVAL);
    }
    if flags & MREMAP_FIXED != 0 && flags & MREMAP_MAYMOVE == 0 {
        return err(EINVAL);
    }
    let Some(old_end) = old_addr
        .checked_add(old_size)
        .and_then(|end| align_up(end, PAGE))
    else {
        return err(EINVAL);
    };
    let Some(new_len) = align_up(new_size, PAGE) else {
        return err(EINVAL);
    };
    let table = crate::mem::kernel_table();
    let Some(vma) = crate::mem::vma::find(table, old_addr) else {
        return err(EFAULT);
    };
    // The range may cover only part of a coalesced VMA (adjacent anonymous
    // mappings merge): split at both boundaries so it is exactly one VMA.
    if old_addr < vma.start || old_end > vma.end {
        return err(EFAULT);
    }
    if vma.start < old_addr {
        crate::mem::vma::split(table, old_addr);
    }
    if old_end < vma.end {
        crate::mem::vma::split(table, old_end);
    }
    let Some(vma) = crate::mem::vma::find(table, old_addr) else {
        return err(EFAULT);
    };
    let old_len = old_end - old_addr;
    let fixed_elsewhere = flags & MREMAP_FIXED != 0 && new_addr != old_addr;

    if !fixed_elsewhere {
        // Shrinking (or the same size): keep the base address and drop the tail.
        if new_len <= old_len {
            let new_end = old_addr + new_len;
            if new_end < old_end {
                unmap_vmas(table, new_end, old_end, true);
                if vma.kind != Kind::Shared {
                    quota::release_for_slot(
                        task::current(),
                        Resource::UserMemory,
                        old_end - new_end,
                    );
                }
            }
            return old_addr;
        }

        // Growing: the pages above the old end are demand-zero (anonymous
        // memory), so a free range above the VMA can be claimed by extending it.
        let delta = new_len - old_len;
        let Some(grow_end) = old_end.checked_add(delta) else {
            return err(ENOMEM);
        };
        let free_above = crate::mem::vma::find_range(table, old_end, grow_end).is_empty();
        if free_above && matches!(vma.kind, Kind::Anon | Kind::Heap) {
            if quota::charge_for_slot(task::current(), Resource::UserMemory, delta).is_err() {
                return err(ENOMEM);
            }
            crate::mem::vma::insert(table, old_addr, grow_end, vma.prot, vma.kind);
            // Keep the bump past a mapping that grew in place, so the next
            // `mmap` does not land on top of it.
            if task::mmap_next() < grow_end {
                task::set_mmap_next(grow_end);
            }
            return old_addr;
        }
    }
    if flags & MREMAP_MAYMOVE == 0 {
        return err(ENOMEM);
    }
    if new_len > old_len && !matches!(vma.kind, Kind::Anon | Kind::Heap) {
        return err(EINVAL); // a stack/file mapping cannot grow by relocation
    }

    let dest = if fixed_elsewhere {
        new_addr
    } else {
        match choose_mremap_dest(table, new_len) {
            Some(dest) => dest,
            None => return err(ENOMEM),
        }
    };
    let Some(dest_end) = dest.checked_add(new_len) else {
        return err(ENOMEM);
    };
    if dest_end > MMAP_LIMIT || (dest < old_end && old_addr < dest_end) {
        return err(EINVAL);
    }
    let extra = new_len.saturating_sub(old_len);
    if extra > 0 && quota::charge_for_slot(task::current(), Resource::UserMemory, extra).is_err() {
        return err(ENOMEM);
    }
    // Replacing a live destination range returns its charge (MAP_FIXED rules).
    let replaced = charged_len(&unmap_vmas(table, dest, dest_end, false));
    // Only `min(old, new)` pages move; a shrinking move drops the old tail.
    let keep = old_len.min(new_len);
    let mut offset = 0;
    while offset < keep {
        if crate::mem::remap_page(table, old_addr + offset, dest + offset).is_err() {
            // Roll the pages already moved back, then undo the charge. The
            // destination was free (or replaced on request), so only the move
            // needs undoing.
            let mut back = 0;
            while back < offset {
                let _ = crate::mem::remap_page(table, dest + back, old_addr + back);
                back += PAGE;
            }
            if extra > 0 {
                quota::release_for_slot(task::current(), Resource::UserMemory, extra);
            }
            return err(ENOMEM);
        }
        offset += PAGE;
    }
    // A file mapping keeps its file: take it out before the old range goes.
    let file = crate::mem::vma::file_of(table, &vma);
    unmap_vmas(table, old_addr, old_end, true);
    match file {
        Some(file) => crate::mem::vma::insert_file(
            table, dest, dest_end, vma.prot, vma.kind, file, vma.file_off,
        ),
        None => crate::mem::vma::insert(table, dest, dest_end, vma.prot, vma.kind),
    }
    if replaced > 0 {
        quota::release_for_slot(task::current(), Resource::UserMemory, replaced);
    }
    if new_len < old_len && vma.kind != Kind::Shared {
        quota::release_for_slot(task::current(), Resource::UserMemory, old_len - new_len);
    }
    if !fixed_elsewhere {
        task::set_mmap_next(dest_end);
    }
    dest
}

/// First free address in the mmap region that fits `len`.
fn choose_mremap_dest(table: PhysAddr, len: u64) -> Option<u64> {
    first_free_range(table, len)
}

/// First-fit search of the mmap region for a hole of `len` bytes.
///
/// It starts at [`MMAP_BASE`] every time rather than at the bump pointer, so a
/// range `munmap` gave back is reused. A bump-only search leaked address space
/// for allocation patterns that map and unmap a large buffer per frame (a
/// 345 KiB clip mask per repaint): the 768 MiB region was gone after ~2300
/// cycles and the process died with `memory allocation failed` although
/// almost no memory was resident.
fn first_free_range(table: PhysAddr, len: u64) -> Option<u64> {
    // One pass over the VMAs in address order: `candidate` only moves forward,
    // past each mapping that overlaps it, so the cost is one scan however
    // large the mappings are.
    let mut vmas = crate::mem::vma::list(table);
    vmas.sort_unstable_by_key(|vma| vma.start);
    let mut candidate = MMAP_BASE;
    for vma in vmas {
        if vma.end <= candidate {
            continue;
        }
        if vma.start >= candidate.checked_add(len)? {
            break;
        }
        candidate = align_up(vma.end, PAGE)?;
    }
    (candidate.checked_add(len)? <= MMAP_LIMIT).then_some(candidate)
}
