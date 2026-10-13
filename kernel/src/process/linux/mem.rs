//! The `mmap` family: `mmap`, `munmap`, `mprotect` and `brk` (`mremap` is
//! [`super::mremap`]'s).
//!
//! All of them work the same way underneath: update the [`crate::mem::vma`]
//! table first (the source of truth for what a range means and who owns the
//! quota charge for it), then make the page tables agree, deferring frames to
//! first touch wherever demand-zero is safe.

use x86_64::PhysAddr;

use crate::mem::vma::{Kind, Prot};
use crate::quota::{self, Resource};
use crate::task;

use super::errno::{err, EINVAL, ENOMEM};
use super::mmapvfs::unmap_vmas;
use super::overcommit::{hopeless, MAP_NORESERVE};
use super::{BRK_LIMIT, MMAP_BASE, MMAP_LIMIT, PAGE};

const MAP_SHARED: u64 = 0x1;
const MAP_SHARED_VALIDATE: u64 = 0x3;
/// The mask of the sharing type in `mmap`'s flags (`MAP_SHARED`,
/// `MAP_PRIVATE`, `MAP_SHARED_VALIDATE`).
const MAP_TYPE: u64 = 0xf;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;

/// Round `value` up to a multiple of `align`, or `None` if the rounding would
/// overflow `u64`. All callers feed this untrusted user lengths, so the
/// addition must be checked: the default profile panics on overflow.
pub(super) fn align_up(value: u64, align: u64) -> Option<u64> {
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
    // More than the machine could ever back is refused now (`ENOMEM`), not at
    // the first touch (an OOM kill): see `overcommit`.
    if flags & MAP_NORESERVE == 0 && hopeless(prot, len) {
        return err(ENOMEM);
    }
    // A reservation (`PROT_NONE`, `MAP_NORESERVE`) commits nothing, so it is
    // charged by the pages that come to be resident (`demand_resolve`), not by
    // its length: a runtime that sets aside gigabytes of address space and
    // fills it in pieces pays for the pieces.
    let reserve = flags & MAP_NORESERVE != 0 || prot.0 == 0;
    // Per-uid user-memory quota (issue #103), charged before any VMA or page
    // table changes so a refusal leaves the address space untouched. The
    // closest Linux errno for "over the user's memory quota" is ENOMEM.
    if !reserve && quota::charge_for_slot(task::current(), Resource::UserMemory, len).is_err() {
        return err(ENOMEM);
    }
    let mut replaced = 0u64;
    if flags & MAP_FIXED != 0 {
        replaced = unmap_vmas(table, base, end, true).charged;
    }
    if reserve {
        crate::mem::vma::insert_reserve(table, base, end, prot, Kind::Anon);
    } else {
        crate::mem::vma::insert(table, base, end, prot, Kind::Anon);
    }
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
    let gone = unmap_vmas(table, start, end, false);
    if !gone.vmas.is_empty() {
        // Unmapping gives the user's quota back (issue #103).
        quota::release_for_slot(task::current(), Resource::UserMemory, gone.charged);
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
                let dropped = crate::mem::unmap_range(table, start, stop) as u64;
                // A reservation is charged by its resident pages: the ones
                // just dropped are no longer held.
                if vma.reserve {
                    quota::release_for_slot(task::current(), Resource::UserMemory, dropped * PAGE);
                }
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
        // The heap is read-write anonymous memory: see `overcommit`.
        if hopeless(Prot::READ | Prot::WRITE, new - current) {
            return current;
        }
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

/// First-fit search of the mmap region for a hole of `len` bytes.
///
/// It starts at [`MMAP_BASE`] every time rather than at the bump pointer, so a
/// range `munmap` gave back is reused. A bump-only search leaked address space
/// for allocation patterns that map and unmap a large buffer per frame (a
/// 345 KiB clip mask per repaint): the 768 MiB region was gone after ~2300
/// cycles and the process died with `memory allocation failed` although
/// almost no memory was resident.
pub(super) fn first_free_range(table: PhysAddr, len: u64) -> Option<u64> {
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
