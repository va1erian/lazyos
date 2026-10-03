//! User address spaces: creation, copy-on-write fork, teardown and fault paths.

use super::*;

/// Create a fresh address space: a new PML4 sharing the kernel's entries
/// (the shared-buffer window and the kernel half, [`USER_PML4_ENTRIES`]..512)
/// with an empty private user window (`0..USER_PML4_ENTRIES`, see
/// [`super::layout`]).
pub fn new_user_table() -> Option<PhysAddr> {
    let phys = alloc_zeroed_frame()?;
    let offset = physical_offset();
    // Safety: the active table and the new frame are mapped.
    unsafe {
        let kernel = active_level_4_table(offset) as *const PageTable as *const u64;
        let table = phys_to_virt(phys).as_mut_ptr::<u64>();
        for i in USER_PML4_ENTRIES..512 {
            core::ptr::write_volatile(table.add(i), core::ptr::read_volatile(kernel.add(i)));
        }
    }
    // The frame allocator may hand back a PML4 of a torn-down address space;
    // `register` resets any stale VMA list keyed by that physical address.
    vma::register(phys);
    Some(phys)
}

/// Map a page into a specific page table.
pub fn map_page_in(table: PhysAddr, virt: VirtAddr, phys: PhysAddr, flags: PageTableFlags) -> bool {
    let offset = physical_offset();
    let table_virt = phys_to_virt(table);
    // Safety: `table` is a PML4 frame we own.
    let level_4 = unsafe { &mut *table_virt.as_mut_ptr::<PageTable>() };
    // Safety: `offset` is the kernel's physical memory mapping offset, which
    // covers every frame `level_4` and its descendants can name.
    let mut mapper = unsafe { OffsetPageTable::new(level_4, offset) };
    let mut frames = GlobalFrames;
    let page = Page::<Size4KiB>::containing_address(virt);
    let frame = PhysFrame::containing_address(phys);
    // Safety: the virtual page is not otherwise mapped in this table.
    unsafe {
        match mapper.map_to(page, frame, flags, &mut frames) {
            Ok(flush) => {
                flush.flush();
                true
            }
            Err(err) => {
                crate::serial_println!("map_page_in {:#x} failed: {:?}", virt.as_u64(), err);
                false
            }
        }
    }
}

/// Switch the active address space.
pub fn switch_to(table: PhysAddr) {
    // Safety: `table` is a valid PML4 whose kernel entries match the current one.
    unsafe {
        Cr3::write(
            PhysFrame::containing_address(table),
            x86_64::registers::control::Cr3Flags::empty(),
        );
    }
}

// Copy-on-write: a software bit in the (otherwise unused) page-table entry flags
// marking a shared, read-only user page. The first writer gets a private copy.
pub(super) const COW_BIT: u64 = 1 << 9;
pub(super) use pte::{
    ADDR as PTE_ADDR, HUGE as PTE_HUGE, NX as PTE_NX, PRESENT as PTE_PRESENT, USER as PTE_USER,
    WRITABLE as PTE_WRITABLE,
};

/// View a page table/frame as an array of raw 64-bit entries.
///
/// # Safety
/// `phys` must be mapped and large enough for the accesses made.
pub(super) unsafe fn entry_table(phys: PhysAddr) -> *mut u64 {
    pte::table(phys)
}

/// Count the user data pages mapped in an address space: a diagnostic walk of
/// the private user window (shared COW pages count once per address space). This is the
/// per-address-space accounting hook, reported when a task is reaped and
/// available to tools alongside [`frame_stats`]; keeping a running per-table
/// count is not worth the bookkeeping yet.
pub fn user_table_frame_count(table: PhysAddr) -> usize {
    // Safety: `table` is a PML4 we own.
    unsafe {
        let p4 = entry_table(table);
        (0..USER_PML4_ENTRIES)
            .map(|index| *p4.add(index))
            .filter(|entry| entry & PTE_PRESENT != 0)
            .map(|entry| count_leaves(entry & PTE_ADDR, 3))
            .sum()
    }
}

/// Count present 4 KiB user leaves below a page table of `level`.
///
/// # Safety
/// `phys` must be a page table of `level`.
pub(super) unsafe fn count_leaves(phys: u64, level: u8) -> usize {
    let mut count = 0;
    let entries = entry_table(PhysAddr::new(phys));
    for i in 0..512 {
        let entry = *entries.add(i);
        if entry & PTE_PRESENT == 0 {
            continue;
        }
        if level == 1 {
            if entry & PTE_USER != 0 {
                count += 1;
            }
        } else if entry & PTE_HUGE == 0 {
            count += count_leaves(entry & PTE_ADDR, level - 1);
        }
    }
    count
}

/// Tear down an address space's user half: shared data frames lose a reference
/// (and return to the pool at zero) and page tables are released. Returns how
/// many frames reached reference count zero.
///
/// Only the private user window (PML4 entries `0..USER_PML4_ENTRIES`) is
/// walked; the entries above are shared with the kernel's table and must
/// never be freed. The PML4 frame itself is released too,
/// so the caller must ensure no other task still uses `table` (e.g. threads
/// created with `clone(CLONE_VM)`).
pub fn free_user_table(table: PhysAddr) -> usize {
    let mut released = 0;
    // Safety: `table` is a PML4 we own and are tearing down.
    unsafe {
        let p4 = entry_table(table);
        for index in 0..USER_PML4_ENTRIES {
            let entry = *p4.add(index);
            if entry & PTE_PRESENT != 0 {
                released += free_table(entry & PTE_ADDR, 3);
            }
        }
    }
    if release_frame(table) == Release::Pooled {
        released += 1;
    }
    // The address space no longer exists: drop its VMA list so a recycled PML4
    // frame cannot inherit it.
    vma::forget(table);
    released
}

/// Release the page tables and data frames below a table of `level`
/// (3=PDPT .. 1=PT), then the table at `phys` itself. Returns the number of
/// frames that reached reference count zero.
///
/// # Safety
/// `phys` must be a page table of `level` that no other address space uses.
pub(super) unsafe fn free_table(phys: u64, level: u8) -> usize {
    let mut released = 0;
    let entries = entry_table(PhysAddr::new(phys));
    for i in 0..512 {
        let entry = *entries.add(i);
        if entry & PTE_PRESENT == 0 {
            continue;
        }
        if level == 1 {
            // A leaf: drop one reference. Non-user leaves are kernel aliases
            // and must not be touched.
            if entry & PTE_USER != 0
                && entry & pte::MMIO == 0
                && release_frame(PhysAddr::new(entry & PTE_ADDR)) == Release::Pooled
            {
                released += 1;
            }
        } else if entry & PTE_HUGE == 0 {
            released += free_table(entry & PTE_ADDR, level - 1);
        } else {
            crate::serial_println!("mem: ignoring huge page at {:#x}", entry & PTE_ADDR);
        }
    }
    if release_frame(PhysAddr::new(phys)) == Release::Pooled {
        released += 1;
    }
    released
}

/// Walk `table` to the 4 KiB leaf for `va`, returning a pointer to its entry.
/// `None` means the path is absent or a huge page covers `va`; this never
/// allocates, so callers that need a page mapped go through [`map_page_in`].
///
/// # Safety
/// `table` must be a live PML4 whose lower levels are stable for the duration
/// of the returned pointer's use (no concurrent address-space teardown).
pub(super) unsafe fn leaf_entry(table: PhysAddr, va: u64) -> Option<*mut u64> {
    let index = |shift: u64| ((va >> shift) & 0x1ff) as usize;
    let p4 = entry_table(table);
    let e4 = *p4.add(index(39));
    if e4 & PTE_PRESENT == 0 {
        return None;
    }
    let p3 = entry_table(PhysAddr::new(e4 & PTE_ADDR));
    let e3 = *p3.add(index(30));
    if e3 & PTE_PRESENT == 0 || e3 & PTE_HUGE != 0 {
        return None;
    }
    let p2 = entry_table(PhysAddr::new(e3 & PTE_ADDR));
    let e2 = *p2.add(index(21));
    if e2 & PTE_PRESENT == 0 || e2 & PTE_HUGE != 0 {
        return None;
    }
    let p1 = entry_table(PhysAddr::new(e2 & PTE_ADDR));
    let entry = p1.add(index(12));
    if *entry & PTE_PRESENT == 0 {
        return None;
    }
    Some(entry)
}

/// The raw entries on `table`'s walk for `va`, PML4 first; the walk stops
/// (leaving zeros) at the first absent or huge entry. A diagnostic view for
/// fault reports: it never allocates or changes anything.
pub fn pte_chain(table: PhysAddr, va: u64) -> [u64; 4] {
    let mut chain = [0u64; 4];
    let mut phys = table;
    for (level, shift) in [39u64, 30, 21, 12].into_iter().enumerate() {
        // Safety: `phys` is `table` or a present, non-huge entry's target
        // read on the previous iteration, so it names a live page table.
        let entry = unsafe { *entry_table(phys).add(((va >> shift) & 0x1ff) as usize) };
        chain[level] = entry;
        if entry & PTE_PRESENT == 0 || (level > 0 && entry & PTE_HUGE != 0) {
            break;
        }
        phys = PhysAddr::new(entry & PTE_ADDR);
    }
    chain
}

/// Resolve a write fault on a COW page: copy the frame and map it writable.
/// Returns true if the fault was handled (caller should resume).
pub fn cow_fault(table: PhysAddr, va: u64) -> bool {
    // Safety: we walk the given PML4, whose entries we own.
    let entry = unsafe { leaf_entry(table, va & !(FRAME_SIZE - 1)) };
    let Some(entry) = entry else {
        return false;
    };
    // Safety: `entry` was just returned by `leaf_entry` as a present leaf in
    // this same table.
    let old = unsafe { *entry };
    if old & PTE_USER == 0 || old & COW_BIT == 0 {
        return false;
    }
    let Some(frame) = alloc_zeroed_frame() else {
        return false;
    };
    copy_frame(PhysAddr::new(old & PTE_ADDR), frame);
    // Safety: `entry` is the same present leaf read above; nothing else can
    // have unmapped it in between (single-threaded fault handling).
    unsafe { *entry = frame.as_u64() | ((old & !PTE_ADDR) & !COW_BIT) | PTE_WRITABLE };
    // The page now lives privately here: release our reference to the shared
    // frame (which frees it if this was the last user).
    free_frame(PhysAddr::new(old & PTE_ADDR));
    x86_64::instructions::tlb::flush(VirtAddr::new(va));
    true
}

/// Drop `[start, end)` from `table`'s user mappings: clear each present leaf
/// and return its frame to the allocator (shared COW frames just lose one
/// reference). Page tables are left in place; [`free_user_table`] reaps them.
/// Returns the number of leaves cleared.
pub fn unmap_range(table: PhysAddr, start: u64, end: u64) -> usize {
    let mut cleared = 0;
    let mut va = start & !(FRAME_SIZE - 1);
    while va < end {
        // Safety: `table` is a live address space and we own its entries.
        if let Some(entry) = unsafe { leaf_entry(table, va) } {
            // Safety: `entry` was just returned as a present leaf in this table.
            let value = unsafe { *entry };
            if value & PTE_USER != 0 {
                // Safety: same `entry`, still valid; nothing else can have
                // unmapped it in between (single-threaded teardown).
                unsafe { *entry = 0 };
                free_frame(PhysAddr::new(value & PTE_ADDR));
                cleared += 1;
                x86_64::instructions::tlb::flush(VirtAddr::new(va));
            }
        }
        va += FRAME_SIZE;
    }
    cleared
}

/// Move the mapping of `old_va` to `new_va` without copying the frame: the
/// physical frame, COW bit and protection move with the PTE. Used by `mremap`
/// to relocate a mapping (`new_va` must be unmapped). `Ok(false)` means the
/// source page was not resident; `Err(())` means the destination page tables
/// could not be allocated (the source was restored).
pub fn remap_page(table: PhysAddr, old_va: u64, new_va: u64) -> Result<bool, ()> {
    let old_page = old_va & !(FRAME_SIZE - 1);
    let new_page = new_va & !(FRAME_SIZE - 1);
    // Safety: `table` is a live address space and we own its entries.
    let Some(old_entry) = (unsafe { leaf_entry(table, old_page) }) else {
        return Ok(false);
    };
    // Safety: `old_entry` was just returned as a present leaf in this table.
    let value = unsafe { *old_entry };
    if value & PTE_USER == 0 {
        return Ok(false);
    }
    // Safety: same entry, still valid; nothing else can have unmapped it in
    // between (single-threaded remap).
    unsafe { *old_entry = 0 };
    x86_64::instructions::tlb::flush(VirtAddr::new(old_page));
    let flags = PageTableFlags::from_bits_truncate(value & !PTE_ADDR);
    if !map_page_in(
        table,
        VirtAddr::new(new_page),
        PhysAddr::new(value & PTE_ADDR),
        flags,
    ) {
        // Safety: the entry is still ours and was cleared just above.
        unsafe { *old_entry = value };
        x86_64::instructions::tlb::flush(VirtAddr::new(old_page));
        return Err(());
    }
    Ok(true)
}

/// Apply `prot` to the present user pages of `[start, end)` in `table`.
/// A COW page is privatized first: its protection is per-address-space, so it
/// must not keep sharing a frame after `mprotect`. Returns false when
/// privatizing needed memory and none was available (pages updated before the
/// failure keep their new protection).
pub fn protect_range(table: PhysAddr, start: u64, end: u64, prot: vma::Prot) -> bool {
    let mut va = start & !(FRAME_SIZE - 1);
    while va < end {
        // Safety: `table` is a live address space and we own its entries.
        if let Some(entry) = unsafe { leaf_entry(table, va) } {
            // Safety: `entry` was just returned as a present leaf in this table.
            let old = unsafe { *entry };
            if old & PTE_USER != 0 {
                let new = if old & COW_BIT != 0 {
                    // The page is shared read-only: copy it before changing the
                    // protection, so this address space gets a private frame.
                    let Some(frame) = alloc_zeroed_frame() else {
                        return false;
                    };
                    copy_frame(PhysAddr::new(old & PTE_ADDR), frame);
                    free_frame(PhysAddr::new(old & PTE_ADDR));
                    frame.as_u64() | (old & !PTE_ADDR & !(PTE_WRITABLE | COW_BIT | PTE_NX))
                } else {
                    old
                };
                // Safety: same `entry`, still valid; nothing else can have
                // unmapped it in between (single-threaded `mprotect`).
                unsafe { *entry = apply_prot(new, prot) };
                x86_64::instructions::tlb::flush(VirtAddr::new(va));
            }
        }
        va += FRAME_SIZE;
    }
    true
}

/// Add the flags `prot` implies to an already present PTE value.
pub(super) fn apply_prot(mut entry: u64, prot: vma::Prot) -> u64 {
    entry &= !(PTE_WRITABLE | PTE_NX);
    if prot.has_write() {
        entry |= PTE_WRITABLE;
    }
    if !prot.has_exec() {
        entry |= PTE_NX;
    }
    entry
}

/// Copy one 4 KiB frame through the physical-memory mapping.
pub(super) fn copy_frame(source: PhysAddr, destination: PhysAddr) {
    // Safety: both frames are mapped and exclusively owned by the caller.
    unsafe {
        core::ptr::copy_nonoverlapping(
            phys_to_virt(source).as_ptr::<u8>(),
            phys_to_virt(destination).as_mut_ptr::<u8>(),
            4096,
        );
    }
}

/// Page-table flags for a VMA protection value. Absent `EXEC` maps as NX
/// (`init` enables EFER.NXE), so stacks, heaps and anonymous memory default to
/// non-executable.
pub fn prot_flags(prot: vma::Prot) -> PageTableFlags {
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if prot.has_write() {
        flags |= PageTableFlags::WRITABLE;
    }
    if !prot.has_exec() {
        flags |= PageTableFlags::NO_EXECUTE;
    }
    flags
}

/// Resolve a not-present page fault by materializing a zeroed page for an
/// `Anon`/`Heap`/`Stack` VMA. Returns true if the fault was handled.
///
/// Only access the VMA permits is granted: a write fault in a read-only range
/// (or any access to `PROT_NONE`) stays unresolved and falls through to the
/// fatal path, where a future signal would be delivered. `File` VMAs are
/// mapped eagerly and never demand-fault; a stack is mapped eagerly only where
/// the loader wrote its start frame, the rest of its reservation fills on
/// first touch.
pub fn demand_fault(table: PhysAddr, va: u64, error: PageFaultErrorCode) -> bool {
    if error.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
        return false; // present but forbidden: not a missing page
    }
    let Some(vma) = vma::find(table, va) else {
        return false;
    };
    if !matches!(
        vma.kind,
        vma::Kind::Anon | vma::Kind::Heap | vma::Kind::Stack
    ) {
        return false;
    }
    if !(vma.prot.has_read() || vma.prot.has_exec()) {
        return false;
    }
    let Some(frame) = alloc_zeroed_frame() else {
        return false;
    };
    let page = VirtAddr::new(va & !(FRAME_SIZE - 1));
    if !map_page_in(table, page, frame, prot_flags(vma.prot)) {
        free_frame(frame);
        return false;
    }
    true
}

/// Per-address-space accounting: `(vsz_bytes, resident_pages)`.
///
/// VSZ is the summed VMA length (what the process has reserved); resident
/// pages are the present 4 KiB user leaves (shared COW pages count once per
/// address space). This is the hook tools/tests use to report VSZ/RSS.
#[allow(dead_code)]
pub fn vma_stats(table: PhysAddr) -> (u64, usize) {
    let vsz = vma::list(table).iter().map(|vma| vma.len()).sum();
    (vsz, user_table_frame_count(table))
}
