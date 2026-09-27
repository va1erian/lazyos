//! In-kernel test harness and suite (issue #62).
//!
//! Compiled only when the image is built with `LAZYOS_TESTS=1`; `kernel_main`
//! then calls [`run`] instead of the normal boot. Every test prints exactly one
//! machine-parseable line to serial:
//!
//! ```text
//! TEST:<name>:PASS
//! TEST:<name>:FAIL:<detail>
//! TEST:SUMMARY:PASS=<n> FAIL=<n>
//! ```
//!
//! `tools/test/run.py` parses those lines and turns them into
//! `docs/test/report.md` + `docs/test/report.json`.
//!
//! The suite covers what is deterministic in kernel context: the frame
//! allocator's public API, address-space construction, copy-on-write fork churn
//! (including a deliberate soak loop), the kernel heap, and task/futex/fd
//! bookkeeping. Allocator-internals tests (e.g. the counters reworked by #54)
//! belong in [`mem_suite`] so they can be added without touching the harness.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use x86_64::structures::idt::PageFaultErrorCode;
use x86_64::PhysAddr;

use crate::mem::vma::{Kind, Prot};
use crate::{mem, process, task};

/// A test body: `Err(detail)` fails the test.
type Test = fn() -> Result<(), String>;

/// The suite, run in order. Task tests run last: they register the kernel task
/// and need the address-space helpers to be exercised first.
const SUITE: &[(&str, Test)] = &[
    (
        "mem_frames_distinct_aligned",
        mem_suite::frames_distinct_aligned,
    ),
    ("mem_zeroed_frame_clear", mem_suite::zeroed_frame_clear),
    (
        "mem_user_table_shares_kernel_half",
        mem_suite::user_table_shares_kernel_half,
    ),
    (
        "mem_cow_clone_copies_on_write",
        mem_suite::cow_clone_copies_on_write,
    ),
    ("mem_soak_cow_fork_churn", mem_suite::soak_cow_fork_churn),
    (
        "mem_vma_split_merge_protect",
        mem_suite::vma_split_merge_protect,
    ),
    (
        "mem_demand_zero_and_munmap",
        mem_suite::demand_zero_and_munmap,
    ),
    ("mem_vma_cow_mprotect", mem_suite::vma_cow_mprotect),
    ("heap_vec_integrity", heap_suite::vec_integrity),
    ("task_kernel_registered", task_suite::kernel_registered),
    (
        "task_block_wake_roundtrip",
        task_suite::block_wake_roundtrip,
    ),
    ("task_fork_reap_churn", task_suite::fork_reap_churn),
    ("task_futex_wait_mismatch", task_suite::futex_wait_mismatch),
    ("task_fd_table", task_suite::fd_table),
    (
        "task_wait_queue_block_wake",
        task_suite::wait_queue_block_wake,
    ),
    (
        "task_wait_queue_deadline_timeout",
        task_suite::wait_queue_deadline_timeout,
    ),
    (
        "task_wait_queue_notify_all_order",
        task_suite::wait_queue_notify_all_order,
    ),
    (
        "task_wait_queue_blocked_not_scheduled",
        task_suite::wait_queue_blocked_not_scheduled,
    ),
    ("ipc_open_distinct", ipc_suite::open_distinct),
    ("ipc_duplicate_rights", ipc_suite::duplicate_rights),
    ("ipc_close_frees", ipc_suite::close_frees),
    ("ipc_quota", ipc_suite::quota),
    ("ipc_acl_default_deny", acl_suite::acl_default_deny),
    ("ipc_acl_allow_rule", acl_suite::acl_allow_rule),
    ("ipc_acl_explicit_deny", acl_suite::acl_explicit_deny),
    (
        "ipc_credentials_default_and_set",
        acl_suite::credentials_default_and_set,
    ),
    (
        "ipc_authorize_denial_audited",
        acl_suite::authorize_denial_audited,
    ),
    ("ipc_audit_ring_wraps", acl_suite::audit_ring_wraps),
];

/// Run the suite, print the results, and halt.
pub fn run() -> ! {
    serial_println!("LazyOS: kernel test mode (LAZYOS_TESTS=1)");
    // Tests build address spaces and task frames, so load the GDT/IDT/PIT as a
    // normal boot does. Interrupts stay disabled and the scheduler never starts.
    crate::arch::init();

    let mut pass = 0usize;
    let mut fail = 0usize;
    for (name, test) in SUITE {
        match test() {
            Ok(()) => {
                pass += 1;
                serial_println!("TEST:{name}:PASS");
            }
            Err(detail) => {
                fail += 1;
                serial_println!("TEST:{name}:FAIL:{}", detail.replace('\n', " "));
            }
        }
    }
    serial_println!("TEST:SUMMARY:PASS={pass} FAIL={fail}");
    crate::halt();
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Raw page-table entry bits (the public CPU-visible layout; COW uses bit 9).
const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
const PTE_ADDR: u64 = 0x000F_FFFF_FFFF_F000;
const COW_BIT: u64 = 1 << 9;

/// First virtual address the suite uses for scratch user mappings.
const TEST_VA: u64 = 0x0040_0000;

macro_rules! check {
    ($cond:expr, $($arg:tt)*) => {
        if !($cond) {
            return Err(format!($($arg)*));
        }
    };
}

/// Walk `table` to the 4 KiB PTE for `va`, if mapped. Scratch mappings are 4 KiB;
/// huge-page entries are not expected.
fn raw_entry(table: PhysAddr, va: u64) -> Option<u64> {
    let mut phys = table.as_u64();
    let mut entry = 0u64;
    for shift in [39u64, 30, 21, 12] {
        let ptr = mem::phys_to_virt(PhysAddr::new(phys)).as_ptr::<u64>();
        // Safety: `phys` is a live page table reachable through the physical map.
        entry = unsafe { ptr.add(((va >> shift) & 0x1ff) as usize).read_volatile() };
        if entry & PTE_PRESENT == 0 {
            return None;
        }
        phys = entry & PTE_ADDR;
    }
    Some(entry)
}

/// Physical frame backing `va` in `table`.
fn frame_of(table: PhysAddr, va: u64) -> Result<u64, String> {
    let entry = raw_entry(table, va).ok_or_else(|| format!("no mapping for {va:#x}"))?;
    Ok(entry & PTE_ADDR)
}

/// Deterministic per-page fill pattern (never all-zero, so a stale zeroed frame
/// cannot pass by accident).
fn fill_frame(phys: u64, seed: u8) {
    let ptr = mem::phys_to_virt(PhysAddr::new(phys)).as_mut_ptr::<u8>();
    for i in 0..4096usize {
        // Safety: the frame is mapped writable through the physical memory map.
        unsafe { ptr.add(i).write_volatile(pattern_byte(seed, i)) };
    }
}

/// Whether `phys` still holds the pattern written by [`fill_frame`].
fn frame_matches(phys: u64, seed: u8) -> bool {
    let ptr = mem::phys_to_virt(PhysAddr::new(phys)).as_ptr::<u8>();
    for i in 0..4096usize {
        // Safety: the frame is mapped readable through the physical memory map.
        let got = unsafe { ptr.add(i).read_volatile() };
        if got != pattern_byte(seed, i) {
            return false;
        }
    }
    true
}

fn pattern_byte(seed: u8, index: usize) -> u8 {
    seed ^ (index as u8).wrapping_mul(31)
}

fn page_seed(iteration: u32, page: usize) -> u8 {
    (iteration as u8).wrapping_mul(31) ^ (page as u8).wrapping_mul(17)
}

fn to_string(error: &'static str) -> String {
    error.into()
}

// ---------------------------------------------------------------------------
// Frame allocator and address spaces
// ---------------------------------------------------------------------------

mod mem_suite {
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
            removed.len() == 1
                && removed[0].start == base + 0x1000
                && removed[0].end == base + 0x2000,
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

        // The child still shares the original frame and can copy on write.
        check!(
            mem::cow_fault(child, base),
            "child cow_fault failed after the parent mprotect"
        );
        let child_phys = frame_of(child, base)?;
        check!(
            child_phys != shared && child_phys != private,
            "child did not get its own copy"
        );
        check!(frame_matches(child_phys, 0x5a), "child copy is corrupted");

        mem::unmap_range(parent, base, base + 4096);
        mem::unmap_range(child, base, base + 4096);
        mem::free_user_table(parent);
        mem::free_user_table(child);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Kernel heap
// ---------------------------------------------------------------------------

mod heap_suite {
    use super::*;

    /// Allocating, filling, dropping and reusing heap blocks keeps data intact.
    pub fn vec_integrity() -> Result<(), String> {
        let mut buffers: Vec<Vec<u8>> = Vec::with_capacity(64);
        for round in 0..64u32 {
            let mut buffer = Vec::with_capacity(4096);
            for index in 0..4096u32 {
                buffer.push((round as u8) ^ (index as u8).wrapping_mul(7));
            }
            buffers.push(buffer);
        }
        for (round, buffer) in buffers.iter().enumerate() {
            check!(
                buffer.len() == 4096,
                "round {round} length is {}",
                buffer.len()
            );
            for (index, &byte) in buffer.iter().enumerate() {
                check!(
                    byte == (round as u8) ^ (index as u8).wrapping_mul(7),
                    "round {round} byte {index} is {byte:#x} (heap corruption?)"
                );
            }
        }
        drop(buffers);

        // Freed blocks should be reusable and still hold what we write.
        let mut reused: Vec<u8> = Vec::with_capacity(4096);
        for index in 0..4096u32 {
            reused.push((index as u8) ^ 0x5a);
        }
        check!(
            reused.len() == 4096,
            "reallocated length is {}",
            reused.len()
        );
        check!(
            reused[2048] == (2048u32 as u8) ^ 0x5a,
            "reallocated buffer is corrupted"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Task bookkeeping
// ---------------------------------------------------------------------------

mod task_suite {
    use super::*;

    /// Registering the kernel task sets the current slot and snapshot fields.
    pub fn kernel_registered() -> Result<(), String> {
        task::register_kernel();
        check!(
            task::current() == task::KERNEL_TASK,
            "current task is {}, expected the kernel slot",
            task::current()
        );
        let (name, _, done) =
            task::snapshot(task::KERNEL_TASK).ok_or("kernel task is not registered")?;
        check!(name == "kernel", "kernel task is named {name:?}");
        check!(!done, "kernel task starts done");
        check!(
            !task::has_children(),
            "kernel task unexpectedly has children"
        );
        check!(
            task::reap_child().is_none(),
            "kernel task reaped a nonexistent child"
        );
        Ok(())
    }

    /// The futex park/wake mechanism: `set_blocked` parks, `wake_task` resumes.
    pub fn block_wake_roundtrip() -> Result<(), String> {
        check!(!task::blocked(), "task starts blocked");
        task::set_blocked(true);
        check!(task::blocked(), "set_blocked(true) did not park the task");
        task::wake_task(task::current());
        check!(!task::blocked(), "wake_task did not resume the task");
        Ok(())
    }

    /// `spawn_fork` bookkeeping, exit, and reaping across many rounds.
    pub fn fork_reap_churn() -> Result<(), String> {
        task::harness::reset();
        for round in 0..8u32 {
            let slot = task::spawn_fork()
                .map_err(|error| format!("round {round}: spawn_fork: {error}"))?;
            check!(
                (1..task::MAX_TASKS).contains(&slot),
                "round {round}: fork slot {slot} is out of range"
            );
            check!(
                task::reap_child().is_none(),
                "round {round}: reaped a child that is still running"
            );
            task::harness::finish(slot, 0x40 + round as u64);
            let (reaped, status) = task::reap_child()
                .ok_or_else(|| format!("round {round}: child is not reapable"))?;
            check!(
                reaped == slot,
                "round {round}: reaped slot {reaped}, expected {slot}"
            );
            check!(
                status == 0x40 + round as u64,
                "round {round}: exit status {status:#x}"
            );
        }
        task::harness::reset();
        Ok(())
    }

    /// `futex(FUTEX_WAIT)` on a mismatched word returns EAGAIN without blocking;
    /// `FUTEX_WAKE` with no waiters returns 0.
    pub fn futex_wait_mismatch() -> Result<(), String> {
        let mut word: u32 = 7;
        let addr = core::ptr::addr_of_mut!(word) as u64;
        let eagain = (-11i64) as u64;
        let result = process::linux::dispatch_for_test(202, addr, 0, 99);
        check!(
            result == eagain,
            "futex WAIT on a mismatched word returned {result:#x}, expected EAGAIN"
        );
        let woken = process::linux::dispatch_for_test(202, addr, 1, 1);
        check!(woken == 0, "futex WAKE with no waiters returned {woken}");
        check!(word == 7, "futex syscall modified the word");
        Ok(())
    }

    /// Open, size, read, seek, duplicate and close a descriptor.
    pub fn fd_table() -> Result<(), String> {
        task::register_kernel();
        let fd = task::fd_open(task::Fd::File {
            data: b"hello".to_vec(),
            offset: 0,
        })
        .ok_or("fd_open failed")?;
        check!(fd >= 3, "fd_open returned reserved slot {fd}");
        check!(
            task::fd_kind(fd) == task::FdKind::File,
            "opened fd is not a file"
        );
        check!(
            task::fd_size(fd) == Some(5),
            "file size is {:?}, expected 5",
            task::fd_size(fd)
        );

        let mut buffer = [0u8; 8];
        let read = task::fd_read(fd, buffer.as_mut_ptr(), buffer.len()).ok_or("fd_read failed")?;
        check!(
            read == 5 && &buffer[..5] == b"hello",
            "fd_read got {read} bytes: {:?}",
            &buffer[..read.min(buffer.len())]
        );

        check!(task::fd_seek(fd, 0, 0) == Some(0), "fd_seek(SET 0) failed");
        let duplicate = task::fd_dup(fd).ok_or("fd_dup failed")?;
        check!(duplicate != fd, "fd_dup reused the same slot");
        check!(
            task::fd_size(duplicate) == Some(5),
            "duplicated fd lost its data"
        );
        check!(task::fd_close(duplicate), "fd_close(duplicate) failed");
        check!(
            task::fd_kind(duplicate) == task::FdKind::Closed,
            "closed fd is still open"
        );
        check!(task::fd_close(fd), "fd_close(fd) failed");
        check!(
            !task::fd_close(fd),
            "closing an already closed fd succeeded"
        );
        Ok(())
    }

    /// A wait queue parks a task and `notify_one` moves it back to `Runnable`
    /// with reason `Woken`, synchronously (no timer tick is involved).
    pub fn wait_queue_block_wake() -> Result<(), String> {
        task::register_kernel();
        let me = task::current();
        let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
        queue.park(me, None);
        check!(
            matches!(
                task::harness::state(me),
                Some(task::TaskState::Blocked { .. })
            ),
            "park did not block the task: {:?}",
            task::harness::state(me)
        );
        check!(queue.notify_one() == 1, "notify_one did not wake a waiter");
        check!(
            task::harness::state(me) == Some(task::TaskState::Runnable),
            "woken task is not runnable: {:?}",
            task::harness::state(me)
        );
        check!(
            task::harness::take_wake_reason(me) == Some(task::WakeReason::Woken),
            "wake reason is not Woken: {:?}",
            task::harness::take_wake_reason(me)
        );
        check!(
            queue.notify_one() == 0,
            "notify_one woke a task that was not parked"
        );
        Ok(())
    }

    /// The deadline sweep leaves a task parked before its deadline and wakes it
    /// with `TimedOut` exactly at the deadline.
    pub fn wait_queue_deadline_timeout() -> Result<(), String> {
        task::register_kernel();
        let me = task::current();
        let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
        let now = task::ticks();
        queue.park(me, Some(now + 10));
        task::harness::expire_deadlines(now + 9);
        check!(
            matches!(
                task::harness::state(me),
                Some(task::TaskState::Blocked { .. })
            ),
            "task woke before its deadline: {:?}",
            task::harness::state(me)
        );
        task::harness::expire_deadlines(now + 10);
        check!(
            task::harness::state(me) == Some(task::TaskState::Runnable),
            "deadline did not wake the task: {:?}",
            task::harness::state(me)
        );
        check!(
            task::harness::take_wake_reason(me) == Some(task::WakeReason::TimedOut),
            "deadline wake reason is not TimedOut: {:?}",
            task::harness::take_wake_reason(me)
        );
        // The sweep leaves the waiter enqueued; a later notify must drop it
        // without counting it as woken.
        check!(
            queue.notify_all() == 0,
            "a timed-out waiter was counted as woken"
        );
        Ok(())
    }

    /// `notify_one` wakes the oldest waiter first; `notify_all` drains the rest.
    pub fn wait_queue_notify_all_order() -> Result<(), String> {
        task::harness::reset();
        let first = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        let second = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        let third = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
        queue.park(first, None);
        queue.park(second, None);
        queue.park(third, None);

        check!(
            queue.notify_one() == 1,
            "notify_one should wake exactly the oldest waiter"
        );
        check!(
            task::harness::state(first) == Some(task::TaskState::Runnable),
            "oldest waiter was not woken first"
        );
        check!(
            task::harness::state(second)
                == Some(task::TaskState::Blocked {
                    wait: task::WaitKind::Sleep,
                    deadline: None
                }),
            "second waiter woke before the first"
        );
        check!(
            task::harness::state(third)
                == Some(task::TaskState::Blocked {
                    wait: task::WaitKind::Sleep,
                    deadline: None
                }),
            "third waiter woke before the first"
        );

        check!(queue.notify_all() == 2, "notify_all did not drain the rest");
        check!(
            task::harness::state(second) == Some(task::TaskState::Runnable)
                && task::harness::state(third) == Some(task::TaskState::Runnable),
            "notify_all left a waiter parked"
        );

        for slot in [first, second, third] {
            task::harness::finish(slot, 0);
            check!(
                task::reap_child().is_some(),
                "child {slot} was not reapable"
            );
        }
        task::harness::reset();
        Ok(())
    }

    /// The scheduler selection skips blocked tasks and picks them again only
    /// after a wake.
    pub fn wait_queue_blocked_not_scheduled() -> Result<(), String> {
        task::harness::reset();
        let child = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
        queue.park(child, None);
        check!(
            task::harness::next_runnable() != child,
            "selection picked blocked task {child}"
        );
        check!(queue.notify_one() == 1, "notify_one did not wake the child");
        check!(
            task::harness::next_runnable() == child,
            "selection did not pick the woken task"
        );
        task::harness::finish(child, 0);
        check!(task::reap_child().is_some(), "child was not reapable");
        task::harness::reset();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Messenger handles
// ---------------------------------------------------------------------------

mod ipc_suite {
    use super::*;
    use crate::ipc::handles::{self, rights, Error, HandleKind, MAX_HANDLES};

    /// Each handle test starts from an empty table in the kernel task's slot.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        handles::reset_for_task(task::current());
        Ok(())
    }

    /// Opening returns distinct handles that resolve to what was stored.
    pub fn open_distinct() -> Result<(), String> {
        fresh()?;
        let object =
            handles::open(HandleKind::Object, rights::CALL, 1).map_err(|error| error.message())?;
        let buffer =
            handles::open(HandleKind::Buffer, rights::ALL, 2).map_err(|error| error.message())?;
        check!(object != buffer, "open reused handle {object}");
        let entry = handles::get(object).map_err(|error| error.message())?;
        check!(
            entry.kind == HandleKind::Object
                && entry.rights == rights::CALL
                && entry.object_id == 1,
            "entry does not match what was opened"
        );
        check!(handles::count() == 2, "count is {}", handles::count());
        handles::reset_for_task(task::current());
        Ok(())
    }

    /// Duplication requires the right and may only narrow rights.
    pub fn duplicate_rights() -> Result<(), String> {
        fresh()?;
        let plain =
            handles::open(HandleKind::Channel, rights::CALL, 7).map_err(|error| error.message())?;
        check!(
            handles::duplicate(plain, rights::CALL) == Err(Error::MissingRight),
            "duplicated a handle without the DUPLICATE right"
        );
        let dupable = handles::open(HandleKind::Channel, rights::CALL | rights::DUPLICATE, 8)
            .map_err(|error| error.message())?;
        check!(
            handles::duplicate(dupable, rights::ALL) == Err(Error::MissingRight),
            "duplication widened the rights (escalation)"
        );
        let copy = handles::duplicate(dupable, rights::CALL).map_err(|error| error.message())?;
        check!(copy != dupable, "duplicate reused the handle");
        check!(
            handles::get(copy).map_err(|error| error.message())?.rights == rights::CALL,
            "duplicate did not take the requested rights"
        );
        handles::reset_for_task(task::current());
        Ok(())
    }

    /// Closing frees the slot and the lowest free slot is reused.
    pub fn close_frees() -> Result<(), String> {
        fresh()?;
        let handle = handles::open(HandleKind::Endpoint, rights::CALL, 3)
            .map_err(|error| error.message())?;
        handles::close(handle).map_err(|error| error.message())?;
        check!(
            handles::get(handle) == Err(Error::InvalidHandle),
            "closed handle still resolves"
        );
        check!(
            handles::close(handle) == Err(Error::InvalidHandle),
            "double close succeeded"
        );
        let again = handles::open(HandleKind::Endpoint, rights::CALL, 4)
            .map_err(|error| error.message())?;
        check!(again == handle, "freed slot was not reused");
        handles::reset_for_task(task::current());
        Ok(())
    }

    /// The per-process quota is enforced, and teardown drops every handle.
    pub fn quota() -> Result<(), String> {
        fresh()?;
        for index in 0..MAX_HANDLES {
            handles::open(HandleKind::Object, rights::CALL, index as u64)
                .map_err(|error| error.message())?;
        }
        check!(
            handles::open(HandleKind::Object, rights::CALL, 0) == Err(Error::NoFreeHandle),
            "handle quota was not enforced"
        );
        handles::reset_for_task(task::current());
        check!(
            handles::count() == 0,
            "reset left {} handles behind",
            handles::count()
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Messenger credentials, ACL, and audit (issue #68)
// ---------------------------------------------------------------------------

mod acl_suite {
    use super::*;
    use crate::ipc::credentials::{self, Cred};
    use crate::ipc::{acl, audit};

    const IFACE: u64 = 0x0102_0304_0506_0708;
    const OTHER_IFACE: u64 = 0x1111_2222_3333_4444;

    /// Every ACL test starts from the bring-up state: root credentials, empty
    /// policy (the bootstrap window), empty audit ring, tracing off.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        credentials::reset_for_task(task::current());
        acl::load(&[]);
        audit::reset();
        audit::set_trace(false);
        Ok(())
    }

    /// With a non-empty policy, a call that matches no rule is denied by
    /// default, with friendly text and the default-deny reason code.
    pub fn acl_default_deny() -> Result<(), String> {
        fresh()?;
        acl::load(&[acl::Rule {
            actor: 1000,
            interface_id: OTHER_IFACE,
            method: 1,
            allow: true,
        }]);
        let decision = acl::evaluate(2000, IFACE, 7);
        check!(decision.denied(), "an unmatched call was allowed");
        let reason = decision.reason().ok_or("denial carried no reason")?;
        check!(!reason.is_empty(), "the denial reason is empty");
        let (decision, code) = acl::evaluate_verdict(2000, IFACE, 7);
        check!(
            decision.denied(),
            "evaluate_verdict allowed an unmatched call"
        );
        check!(
            code == acl::reason::DEFAULT_DENY,
            "unmatched call has reason code {code}, expected default deny"
        );
        Ok(())
    }

    /// A matching allow rule permits exactly its `(actor, interface, method)`
    /// triple; neighbours still fall through to default deny.
    pub fn acl_allow_rule() -> Result<(), String> {
        fresh()?;
        acl::load(&[acl::Rule {
            actor: 1000,
            interface_id: IFACE,
            method: 7,
            allow: true,
        }]);
        check!(
            acl::evaluate(1000, IFACE, 7) == acl::Decision::Allow,
            "a matching allow rule was not honored"
        );
        check!(
            acl::evaluate(1001, IFACE, 7).denied(),
            "the rule leaked to a different uid"
        );
        check!(
            acl::evaluate(1000, IFACE, 8).denied(),
            "the rule leaked to a different method"
        );
        check!(
            acl::evaluate(1000, OTHER_IFACE, 7).denied(),
            "the rule leaked to a different interface"
        );
        Ok(())
    }

    /// An explicit deny rule wins over a later allow-all, and the wildcard
    /// allow still covers actors the deny does not name.
    pub fn acl_explicit_deny() -> Result<(), String> {
        fresh()?;
        acl::load(&[
            acl::Rule {
                actor: 1000,
                interface_id: IFACE,
                method: 7,
                allow: false,
            },
            acl::Rule {
                actor: acl::ANY_ACTOR,
                interface_id: acl::ANY_INTERFACE,
                method: acl::ANY_METHOD,
                allow: true,
            },
        ]);
        let (decision, code) = acl::evaluate_verdict(1000, IFACE, 7);
        check!(decision.denied(), "an explicit deny rule was overridden");
        check!(
            code == acl::reason::EXPLICIT_DENY,
            "explicit deny has reason code {code}"
        );
        check!(
            acl::evaluate(2000, IFACE, 7) == acl::Decision::Allow,
            "the wildcard allow rule was not honored"
        );
        Ok(())
    }

    /// Credentials default to root and `set`/`set_current` replace them
    /// per slot; the ACL authority is the uid.
    pub fn credentials_default_and_set() -> Result<(), String> {
        fresh()?;
        check!(
            credentials::of(task::current()) == Cred::ROOT,
            "default credentials are not root: {:?}",
            credentials::of(task::current())
        );
        check!(
            Cred::ROOT.uid == 0 && Cred::ROOT.has_cap(credentials::CAP_SYS_ADMIN),
            "root is not uid 0 with CAP_SYS_ADMIN"
        );
        let cred = Cred::new(1000, 100, credentials::CAP_AUDIT_READ, 7, 0xabc);
        credentials::set_current(cred);
        check!(
            credentials::of(task::current()) == cred,
            "set_current did not replace the credentials"
        );
        check!(
            credentials::of(task::current()).authority() == 1000,
            "the ACL authority is not the uid"
        );
        credentials::reset_for_task(task::current());
        check!(
            credentials::of(task::current()) == Cred::ROOT,
            "reset did not restore root"
        );
        Ok(())
    }

    /// `authorize` reads the actor's credentials, denies by default, and always
    /// records the denial with its correlation id; the hash chain advances.
    /// Untraced allows are not recorded; traced ones are.
    pub fn authorize_denial_audited() -> Result<(), String> {
        fresh()?;
        let slot = task::current();
        credentials::set(slot, Cred::new(1000, 100, 0, 3, 0));
        acl::load(&[acl::Rule {
            actor: 1000,
            interface_id: OTHER_IFACE,
            method: 1,
            allow: true,
        }]);
        check!(!audit::trace(), "tracing starts enabled");

        let before = audit::last_hash();
        let count_before = audit::count();
        let decision = crate::ipc::authorize(slot, IFACE, 7, 0xfeed);
        check!(decision.denied(), "authorize allowed an unpermitted call");
        check!(
            audit::count() == count_before + 1,
            "authorize did not record the denial"
        );
        let event = *audit::recent(1)
            .first()
            .ok_or("the denial left no audit event")?;
        check!(
            event.uid == 1000 && event.actor_slot == slot && event.txn_id == 0xfeed,
            "the audit event lost the actor or correlation id: {event:?}"
        );
        check!(
            !event.allow,
            "the recorded denial says the call was allowed"
        );
        check!(
            event.reason_code == acl::reason::DEFAULT_DENY,
            "recorded reason code is {}",
            event.reason_code
        );
        let hash = audit::last_hash();
        check!(hash != before, "the hash chain did not advance");
        check!(
            hash == audit::chain(before, &event),
            "the chain head does not match the recorded event"
        );

        // An allow with tracing off is not recorded.
        credentials::set(slot, Cred::ROOT);
        acl::load(&[acl::Rule {
            actor: acl::ANY_ACTOR,
            interface_id: acl::ANY_INTERFACE,
            method: acl::ANY_METHOD,
            allow: true,
        }]);
        check!(
            !crate::ipc::authorize(slot, IFACE, 7, 1).denied(),
            "root was denied by an allow-all policy"
        );
        check!(
            audit::count() == count_before + 1,
            "an untraced allow was recorded"
        );

        // With tracing on, the allow is recorded too.
        audit::set_trace(true);
        check!(
            !crate::ipc::authorize(slot, IFACE, 7, 2).denied(),
            "the traced allow was denied"
        );
        check!(
            audit::count() == count_before + 2,
            "a traced allow was not recorded"
        );
        let last = *audit::recent(1).first().ok_or("no audit event")?;
        check!(
            last.allow && last.txn_id == 2,
            "the traced allow record is wrong: {last:?}"
        );
        Ok(())
    }

    /// The ring overwrites the oldest event when it wraps, always keeps the
    /// newest entries in order, and the chain keeps advancing.
    pub fn audit_ring_wraps() -> Result<(), String> {
        fresh()?;
        let extra = 3usize;
        for index in 0..(audit::AUDIT_CAPACITY + extra) {
            audit::record(audit::AuditEvent {
                ticks: index as u64,
                actor_slot: 1,
                uid: 1000,
                label_id: 0,
                interface_id: IFACE,
                method: index as u32,
                allow: false,
                reason_code: acl::reason::DEFAULT_DENY,
                txn_id: index as u64,
            });
        }
        check!(
            audit::count() == audit::AUDIT_CAPACITY,
            "the ring holds {} events, expected {}",
            audit::count(),
            audit::AUDIT_CAPACITY
        );
        check!(
            audit::total() == (audit::AUDIT_CAPACITY + extra) as u64,
            "the ring total is {}",
            audit::total()
        );
        let recent = audit::recent(extra);
        check!(
            recent.len() == extra,
            "recent returned {} events, expected {extra}",
            recent.len()
        );
        for (i, event) in recent.iter().enumerate() {
            let expected = (audit::AUDIT_CAPACITY + extra - 1 - i) as u64;
            check!(
                event.ticks == expected && event.txn_id == expected,
                "recent[{i}] is ticks {} txn {}, expected {expected}",
                event.ticks,
                event.txn_id
            );
        }
        check!(
            audit::recent(audit::AUDIT_CAPACITY + 10).len() == audit::AUDIT_CAPACITY,
            "recent returned more events than the ring holds"
        );
        Ok(())
    }
}
