//! Frame distinctness/alignment, the shared kernel half of a fresh
//! user table, and copy-on-write fork (including a soak loop).

use super::*;

/// `alloc_frame` hands out distinct, 4 KiB-aligned frames outside low memory.
///
/// #54 replaces the bump allocator; keep this on the public API only, and
/// add counter assertions (e.g. `allocated()`) in a new test here.
pub fn frames_distinct_aligned() -> Result<(), String> {
    let mut seen: Vec<u64> = Vec::new();
    for index in 0..64 {
        let phys = mem::alloc_frame()
            .ok_or_else(|| format!("frame {index}: alloc_frame returned None"))?;
        let address = phys.as_u64();
        check!(
            address & 0xfff == 0,
            "frame {index} {address:#x} is not 4 KiB aligned"
        );
        check!(
            address >= 0x10_0000,
            "frame {index} {address:#x} is in low memory"
        );
        check!(
            !seen.contains(&address),
            "frame {index} {address:#x} was handed out twice"
        );
        seen.push(address);
    }
    Ok(())
}

/// `alloc_zeroed_frame` maps a clear frame readable/writable via `phys_to_virt`.
pub fn zeroed_frame_clear() -> Result<(), String> {
    let phys = mem::alloc_zeroed_frame().ok_or("alloc_zeroed_frame returned None")?;
    let ptr = mem::phys_to_virt(phys).as_mut_ptr::<u8>();
    for i in 0..4096usize {
        // Safety: the frame is freshly allocated and mapped writable.
        let byte = unsafe { ptr.add(i).read_volatile() };
        check!(byte == 0, "zeroed frame byte {i} is {byte:#x}");
    }
    for i in (0..4096).step_by(64) {
        // Safety: as above.
        unsafe { ptr.add(i).write_volatile((i & 0xff) as u8) };
    }
    for i in (0..4096).step_by(64) {
        // Safety: as above.
        let got = unsafe { ptr.add(i).read_volatile() };
        check!(
            got == (i & 0xff) as u8,
            "frame round-trip at {i} got {got:#x}"
        );
    }
    Ok(())
}

/// A fresh user table shares every kernel-half PML4 entry and has an empty
/// user half.
pub fn user_table_shares_kernel_half() -> Result<(), String> {
    let table = mem::new_user_table().ok_or("new_user_table returned None")?;
    let kernel = mem::kernel_table();
    // Safety: both are live PML4 frames mapped through the physical map.
    let kernel_entries =
        unsafe { core::slice::from_raw_parts(mem::phys_to_virt(kernel).as_ptr::<u64>(), 512) };
    // Safety: as above.
    let user_entries =
        unsafe { core::slice::from_raw_parts(mem::phys_to_virt(table).as_ptr::<u64>(), 512) };
    check!(
        user_entries[0] & PTE_PRESENT == 0,
        "new user table entry 0 is present: {:#x}",
        user_entries[0]
    );
    for index in 1..512 {
        check!(
            user_entries[index] == kernel_entries[index],
            "PML4 entry {index} differs: kernel {:#x}, new table {:#x}",
            kernel_entries[index],
            user_entries[index]
        );
    }
    Ok(())
}

/// COW fork: clone marks both sides read-only, and the first writer on each
/// side gets a private copy with the original contents.
pub fn cow_clone_copies_on_write() -> Result<(), String> {
    let parent = mem::new_user_table().ok_or("new_user_table failed")?;
    let pages = process::map_range(parent, TEST_VA, TEST_VA + 2 * 4096).map_err(to_string)?;
    check!(
        pages.len() == 2,
        "map_range mapped {} pages, expected 2",
        pages.len()
    );
    for (index, (_, phys)) in pages.iter().enumerate() {
        fill_frame(*phys, page_seed(0, index));
    }

    let child = mem::clone_user_table(parent).ok_or("clone_user_table failed")?;
    for (index, (va, parent_phys)) in pages.iter().enumerate() {
        let parent_entry =
            raw_entry(parent, *va).ok_or_else(|| format!("parent lost page {index}"))?;
        let child_entry =
            raw_entry(child, *va).ok_or_else(|| format!("child missing page {index}"))?;
        check!(
            parent_entry & PTE_WRITABLE == 0 && parent_entry & COW_BIT != 0,
            "parent page {index} is not COW read-only after clone: {parent_entry:#x}"
        );
        check!(
            child_entry & PTE_WRITABLE == 0 && child_entry & COW_BIT != 0,
            "child page {index} is not COW read-only after clone: {child_entry:#x}"
        );
        check!(
            child_entry & PTE_ADDR == *parent_phys,
            "child page {index} does not share the parent frame"
        );
    }

    for (index, (va, shared_phys)) in pages.iter().enumerate() {
        let seed = page_seed(0, index);
        check!(
            mem::cow_fault(child, *va),
            "cow_fault failed on the child, page {index}"
        );
        let child_phys = frame_of(child, *va)?;
        check!(
            child_phys != *shared_phys,
            "child page {index} still shares the parent frame"
        );
        check!(
            frame_matches(child_phys, seed),
            "child copy of page {index} is corrupted"
        );
        check!(
            frame_matches(*shared_phys, seed),
            "parent frame for page {index} changed"
        );

        check!(
            mem::cow_fault(parent, *va),
            "cow_fault failed on the parent, page {index}"
        );
        let parent_phys = frame_of(parent, *va)?;
        check!(
            parent_phys != child_phys && parent_phys != *shared_phys,
            "parent page {index} was not copied"
        );
        check!(
            frame_matches(parent_phys, seed),
            "parent copy of page {index} is corrupted"
        );
    }

    // A write through the child's private copy must not reach the parent.
    let (va, _) = pages[0];
    let child_phys = frame_of(child, va)?;
    let scratch = mem::phys_to_virt(PhysAddr::new(child_phys)).as_mut_ptr::<u8>();
    // Safety: the frame is private to the child at this point.
    unsafe { scratch.write_volatile(0xEE) };
    check!(
        frame_matches(frame_of(parent, va)?, page_seed(0, 0)),
        "writing the child copy modified the parent"
    );
    Ok(())
}

/// Soak: 500 fork/COW/write cycles, both directions, with progress and a
/// cycle-budget verdict. The runner's wall-clock timeout is the second bound.
pub fn soak_cow_fork_churn() -> Result<(), String> {
    const ITERATIONS: u32 = 500;
    const PAGES: u64 = 3;
    /// A deliberately generous ceiling (roughly a minute of wall clock);
    /// the loop is expected to take well under a second even under TCG.
    const MAX_CYCLES: u64 = 200_000_000_000;

    let start = unsafe { core::arch::x86_64::_rdtsc() };
    for iteration in 0..ITERATIONS {
        let parent = mem::new_user_table()
            .ok_or_else(|| format!("iteration {iteration}: new_user_table failed"))?;
        let pages = process::map_range(parent, TEST_VA, TEST_VA + PAGES * 4096)
            .map_err(|error| format!("iteration {iteration}: {error}"))?;
        for (index, (_, phys)) in pages.iter().enumerate() {
            fill_frame(*phys, page_seed(iteration, index));
        }

        let child = mem::clone_user_table(parent)
            .ok_or_else(|| format!("iteration {iteration}: clone_user_table failed"))?;
        for (index, (va, shared_phys)) in pages.iter().enumerate() {
            let seed = page_seed(iteration, index);
            check!(
                mem::cow_fault(child, *va),
                "iteration {iteration}: child cow_fault failed, page {index}"
            );
            let child_phys = frame_of(child, *va)?;
            check!(
                child_phys != *shared_phys,
                "iteration {iteration}: child page {index} not copied"
            );
            check!(
                frame_matches(child_phys, seed),
                "iteration {iteration}: child copy of page {index} corrupted"
            );

            check!(
                mem::cow_fault(parent, *va),
                "iteration {iteration}: parent cow_fault failed, page {index}"
            );
            let parent_phys = frame_of(parent, *va)?;
            check!(
                parent_phys != child_phys && parent_phys != *shared_phys,
                "iteration {iteration}: parent page {index} not copied"
            );
            check!(
                frame_matches(parent_phys, seed),
                "iteration {iteration}: parent copy of page {index} corrupted"
            );
        }
        if iteration % 100 == 0 {
            serial_println!(
                "TEST:mem_soak_cow_fork_churn:PROGRESS:iteration {iteration}/{ITERATIONS}"
            );
        }
    }
    let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
    serial_println!(
        "TEST:mem_soak_cow_fork_churn:INFO:iterations={ITERATIONS} pages={PAGES} cycles={cycles}"
    );
    check!(
        cycles < MAX_CYCLES,
        "soak used {cycles} cycles, over the {MAX_CYCLES} budget"
    );
    Ok(())
}
