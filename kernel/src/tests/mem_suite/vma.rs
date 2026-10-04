//! VMA split/merge/protect and demand-zero mappings, including
//! `mprotect` over a copy-on-write range.

use super::*;

/// VMA bookkeeping: adjacent inserts with the same protection coalesce,
/// `protect` splits at the range boundary and re-merges, and `remove`
/// leaves a hole (the split `munmap` relies on).
pub fn vma_split_merge_protect() -> Result<(), String> {
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    let base = TEST_VA;
    let rw = Prot::READ | Prot::WRITE;

    mem::vma::insert(table, base, base + 0x1000, rw, Kind::Anon);
    mem::vma::insert(table, base + 0x1000, base + 0x3000, rw, Kind::Anon);
    let list = mem::vma::list(table);
    check!(
        list.len() == 1,
        "adjacent same-prot VMAs did not merge: {} entries",
        list.len()
    );
    check!(
        list[0].start == base && list[0].end == base + 0x3000,
        "merged VMA is {:#x}..{:#x}",
        list[0].start,
        list[0].end
    );
    check!(
        mem::vma::find(table, base + 0x2500).is_some(),
        "find missed an address inside the VMA"
    );
    check!(
        mem::vma::find(table, base + 0x3000).is_none(),
        "find matched the exclusive end"
    );

    check!(
        mem::vma::protect(table, base + 0x1000, base + 0x2000, Prot::READ),
        "protect missed the covered range"
    );
    let list = mem::vma::list(table);
    check!(
        list.len() == 3,
        "protect did not split the VMA: {} entries",
        list.len()
    );
    check!(
        list[1].prot == Prot::READ
            && list[1].start == base + 0x1000
            && list[1].end == base + 0x2000,
        "middle VMA is {list:?}"
    );
    check!(
        mem::vma::find_range(table, base + 0x800, base + 0x1800).len() == 2,
        "find_range did not clip to the covered VMAs"
    );

    mem::vma::protect(table, base + 0x1000, base + 0x2000, rw);
    check!(
        mem::vma::list(table).len() == 1,
        "restoring the protection did not re-merge"
    );

    let removed = mem::vma::remove(table, base + 0x1000, base + 0x2000);
    check!(
        removed.len() == 1 && removed[0].start == base + 0x1000 && removed[0].end == base + 0x2000,
        "remove reported the wrong pieces: {removed:?}"
    );
    check!(
        mem::vma::find(table, base + 0x1000).is_none(),
        "removed address is still in a VMA"
    );
    check!(
        mem::vma::list(table).len() == 2,
        "remove did not split the VMA"
    );
    check!(
        !mem::vma::protect(table, base + 0x9000, base + 0xa000, Prot::READ),
        "protect of an unmapped range reported coverage"
    );

    mem::free_user_table(table);
    check!(
        mem::vma::list(table).is_empty(),
        "free_user_table left VMAs behind"
    );
    Ok(())
}

/// Demand-zero: an `Anon`/`Heap` VMA resolves a missing write fault with a
/// zeroed page; `munmap` drops the mapping and a fault in the hole is no
/// longer ours to fix. File and `PROT_NONE` ranges are never demand-mapped.
pub fn demand_zero_and_munmap() -> Result<(), String> {
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    let base = TEST_VA;
    let rw = Prot::READ | Prot::WRITE;
    let write_fault = PageFaultErrorCode::CAUSED_BY_WRITE;

    mem::vma::insert(table, base, base + 2 * 4096, rw, Kind::Anon);
    check!(
        raw_entry(table, base).is_none(),
        "a lazy VMA was mapped eagerly"
    );

    check!(
        mem::demand_fault(table, base, write_fault),
        "demand write fault was not resolved"
    );
    let entry = raw_entry(table, base).ok_or("no mapping after the demand fault")?;
    check!(
        entry & PTE_WRITABLE != 0,
        "demand page is not writable: {entry:#x}"
    );
    check!(
        entry & (1 << 63) != 0,
        "anonymous memory is executable: {entry:#x}"
    );
    let phys = entry & PTE_ADDR;
    let ptr = mem::phys_to_virt(PhysAddr::new(phys)).as_ptr::<u8>();
    for i in (0..4096).step_by(64) {
        // Safety: the frame is mapped readable through the physical map.
        let byte = unsafe { ptr.add(i).read_volatile() };
        check!(byte == 0, "demand page byte {i} is {byte:#x} (not zeroed)");
    }

    let (vsz, resident) = mem::vma_stats(table);
    check!(vsz == 2 * 4096, "VSZ is {vsz}, expected 8192");
    check!(resident == 1, "resident pages are {resident}, expected 1");

    // munmap the faulted page: the mapping goes and its VMA piece too.
    let removed = mem::vma::remove(table, base, base + 4096);
    check!(!removed.is_empty(), "munmap removed no VMA");
    check!(
        mem::unmap_range(table, base, base + 4096) == 1,
        "unmap_range did not clear the resident page"
    );
    check!(
        raw_entry(table, base).is_none(),
        "page still mapped after munmap"
    );
    check!(
        !mem::demand_fault(table, base, write_fault),
        "demand fault filled a munmapped hole"
    );

    // The remaining page still faults in, on a plain read this time.
    check!(
        mem::demand_fault(table, base + 4096, PageFaultErrorCode::empty()),
        "demand read fault was not resolved"
    );
    check!(
        raw_entry(table, base + 4096).is_some(),
        "second page missing after a read fault"
    );

    // Only Anon/Heap is demand-zero, and PROT_NONE permits no access.
    mem::vma::insert(table, base + 8192, base + 12288, rw, Kind::File);
    check!(
        !mem::demand_fault(table, base + 8192, PageFaultErrorCode::empty()),
        "a File VMA was demand-mapped"
    );
    mem::vma::insert(table, base + 12288, base + 16384, Prot(0), Kind::Anon);
    check!(
        !mem::demand_fault(table, base + 12288, write_fault),
        "a PROT_NONE VMA was demand-mapped"
    );

    mem::unmap_range(table, base, base + 2 * 4096);
    mem::free_user_table(table);
    Ok(())
}

/// Fork + `mprotect` interaction: cloning copies the VMA list and marks
/// pages COW; `protect_range` privatizes a COW page before applying the new
/// flags, so the two address spaces stop sharing.
pub fn vma_cow_mprotect() -> Result<(), String> {
    let parent = mem::new_user_table().ok_or("new_user_table failed")?;
    let base = TEST_VA;
    let rw = Prot::READ | Prot::WRITE;
    mem::vma::insert(parent, base, base + 4096, rw, Kind::Heap);
    check!(
        mem::demand_fault(parent, base, PageFaultErrorCode::CAUSED_BY_WRITE),
        "demand fault failed"
    );
    let shared = frame_of(parent, base)?;
    fill_frame(shared, 0x5a);

    let child = mem::clone_user_table(parent).ok_or("clone_user_table failed")?;
    let copied = mem::vma::Vma {
        start: base,
        end: base + 4096,
        prot: rw,
        kind: Kind::Heap,
    };
    let child_list = mem::vma::list(child);
    check!(
        child_list == [copied],
        "fork did not copy the VMA list: {child_list:?}"
    );
    let parent_entry = raw_entry(parent, base).ok_or("parent lost its page")?;
    let child_entry = raw_entry(child, base).ok_or("child lost the shared page")?;
    check!(
        parent_entry & PTE_WRITABLE == 0 && parent_entry & COW_BIT != 0,
        "parent is not COW read-only: {parent_entry:#x}"
    );
    check!(
        child_entry & PTE_ADDR == shared,
        "child does not share the parent frame"
    );

    // mprotect(read) on the COW page: private copy, write cleared, COW gone.
    check!(
        mem::protect_range(parent, base, base + 4096, Prot::READ),
        "protect_range failed"
    );
    check!(
        mem::vma::protect(parent, base, base + 4096, Prot::READ),
        "VMA protect missed the range"
    );
    let parent_entry = raw_entry(parent, base).ok_or("parent page vanished")?;
    let private = parent_entry & PTE_ADDR;
    check!(
        private != shared,
        "parent still shares the frame after mprotect"
    );
    check!(
        parent_entry & PTE_WRITABLE == 0 && parent_entry & COW_BIT == 0,
        "mprotect flags are {parent_entry:#x}"
    );
    check!(frame_matches(private, 0x5a), "privatized copy is corrupted");
    check!(
        frame_matches(shared, 0x5a),
        "mprotect modified the still-shared frame"
    );
    check!(
        mem::vma::find(parent, base).map(|vma| vma.prot) == Some(Prot::READ),
        "the parent VMA did not take the new protection"
    );

    // The child is now the original frame's only owner: its write fault
    // keeps that frame, made writable in place (P6.6), not a copy.
    check!(
        mem::cow_fault(child, base),
        "child cow_fault failed after the parent mprotect"
    );
    let child_phys = frame_of(child, base)?;
    let child_entry = raw_entry(child, base).ok_or("child page vanished")?;
    check!(
        child_phys == shared
            && child_phys != private
            && child_entry & PTE_WRITABLE != 0
            && child_entry & COW_BIT == 0,
        "the child, sole owner of the frame, was copied or left read-only: {child_entry:#x}"
    );
    check!(frame_matches(child_phys, 0x5a), "child page is corrupted");

    mem::unmap_range(parent, base, base + 4096);
    mem::unmap_range(child, base, base + 4096);
    mem::free_user_table(parent);
    mem::free_user_table(child);
    Ok(())
}
