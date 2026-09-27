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
    ("task_process_tree_fork", task_suite::process_tree_fork),
    ("task_pgid_sid_inherit", task_suite::pgid_sid_inherit),
    ("task_setsid_new_session", task_suite::setsid_new_session),
    ("task_reparent_on_death", task_suite::reparent_on_death),
    (
        "task_kill_group_terminates",
        task_suite::kill_group_terminates,
    ),
    (
        "task_process_list_snapshot",
        task_suite::process_list_snapshot,
    ),
    (
        "task_signal_block_unblock",
        signal_suite::block_unblock_pending,
    ),
    (
        "task_signal_kill_wakes_sleeper",
        signal_suite::kill_wakes_blocked,
    ),
    (
        "task_signal_kill_uncatchable",
        signal_suite::sigkill_uncatchable,
    ),
    (
        "task_signal_sigchld_child_exit",
        signal_suite::sigchld_on_child_exit,
    ),
    (
        "task_signal_handler_frame_roundtrip",
        signal_suite::handler_frame_roundtrip,
    ),
    ("task_signal_stop_continue", signal_suite::stop_continue),
    ("ipc_open_distinct", ipc_suite::open_distinct),
    ("ipc_duplicate_rights", ipc_suite::duplicate_rights),
    ("ipc_close_frees", ipc_suite::close_frees),
    ("ipc_quota", ipc_suite::quota),
    (
        "ipc_channel_echo_roundtrip",
        ipc_channel_suite::echo_roundtrip,
    ),
    (
        "ipc_channel_one_way_order_and_limits",
        ipc_channel_suite::one_way_order_and_limits,
    ),
    (
        "ipc_channel_deadline_timeout",
        ipc_channel_suite::deadline_timeout,
    ),
    (
        "ipc_channel_deadline_reply_race",
        ipc_channel_suite::deadline_reply_race,
    ),
    (
        "ipc_channel_call_deadline_zero",
        ipc_channel_suite::call_deadline_zero,
    ),
    ("ipc_channel_cancel_wakes", ipc_channel_suite::cancel_wakes),
    ("ipc_channel_peer_died", ipc_channel_suite::peer_died),
    (
        "ipc_channel_deadlock_refused",
        ipc_channel_suite::deadlock_refused,
    ),
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
    use crate::task::process::GroupError;

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

    /// Build `depth` nested `spawn_fork` children (`spawn_fork` forks the
    /// current task, so the harness points `current()` at each new child) and
    /// return their slots from root to leaf. Leaves `current()` at the leaf.
    fn fork_chain(depth: usize) -> Result<Vec<usize>, String> {
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        let mut chain = Vec::new();
        for level in 0..depth {
            let slot = task::spawn_fork().map_err(|error| format!("level {level}: {error}"))?;
            chain.push(slot);
            task::harness::switch_current(slot);
        }
        Ok(chain)
    }

    /// Mark `slots` finished leaf-first (so each death re-parents its children)
    /// and reap every one of them as init, then reset the table.
    fn finish_and_reap_all(slots: &[usize]) -> Result<(), String> {
        for &slot in slots.iter().rev() {
            task::harness::finish(slot, 0);
        }
        task::harness::switch_current(task::KERNEL_TASK);
        let mut reaped = 0;
        while task::reap_child().is_some() {
            reaped += 1;
        }
        check!(
            reaped == slots.len(),
            "reaped {reaped} of {} finished tasks",
            slots.len()
        );
        task::harness::reset();
        Ok(())
    }

    /// `spawn_fork` chains form a tree: parent links, children derivation and
    /// the introspection rows all agree.
    pub fn process_tree_fork() -> Result<(), String> {
        let chain = fork_chain(3)?;
        task::harness::switch_current(task::KERNEL_TASK);
        let (root, child, leaf) = (chain[0], chain[1], chain[2]);

        check!(
            task::process::ppid_of(root) == 0,
            "root ppid is {}, expected init",
            task::process::ppid_of(root)
        );
        check!(
            task::process::ppid_of(child) == root,
            "child ppid is {}, expected {root}",
            task::process::ppid_of(child)
        );
        check!(
            task::process::ppid_of(leaf) == child,
            "leaf ppid is {}, expected {child}",
            task::process::ppid_of(leaf)
        );
        check!(
            task::process::children_of(task::KERNEL_TASK) == [root],
            "init children are {:?}, expected [{root}]",
            task::process::children_of(task::KERNEL_TASK)
        );
        check!(
            task::process::children_of(root) == [child],
            "root children are {:?}, expected [{child}]",
            task::process::children_of(root)
        );
        check!(
            task::process::children_of(leaf).is_empty(),
            "leaf unexpectedly has children: {:?}",
            task::process::children_of(leaf)
        );
        check!(
            task::process::find_by_pid(child) == Some(child),
            "find_by_pid({child}) missed the live child"
        );

        // The same tree, seen through the introspection API.
        let list = task::process::process_list();
        check!(
            list.len() == 4,
            "process_list has {} rows, expected 4",
            list.len()
        );
        let row = list
            .iter()
            .find(|row| row.pid == leaf)
            .ok_or("leaf missing from process_list")?;
        check!(
            row.slot == leaf
                && row.ppid == child
                && row.pgid == root
                && row.sid == root
                && row.uid == 0
                && row.state == task::TaskState::Runnable
                && row.name == "fork",
            "leaf row is {row:?}"
        );
        finish_and_reap_all(&chain)
    }

    /// `fork` inherits the parent's pgid/sid; a non-leader child can form its
    /// own group, and the group/session errors match Linux.
    pub fn pgid_sid_inherit() -> Result<(), String> {
        let chain = fork_chain(3)?;
        task::harness::switch_current(task::KERNEL_TASK);
        let (root, child, leaf) = (chain[0], chain[1], chain[2]);

        check!(
            task::process::pgid_of(root) == root && task::process::sid_of(root) == root,
            "root is not its own leader (pgid {}, sid {})",
            task::process::pgid_of(root),
            task::process::sid_of(root)
        );
        for slot in [child, leaf] {
            check!(
                task::process::pgid_of(slot) == root && task::process::sid_of(slot) == root,
                "slot {slot} did not inherit the root group/session (pgid {}, sid {})",
                task::process::pgid_of(slot),
                task::process::sid_of(slot)
            );
        }

        // A child (not a session leader) can form a new group in the session.
        task::process::setpgid(root, child as i64, child as i64)
            .map_err(|error| format!("setpgid(child, child): {error:?}"))?;
        check!(
            task::process::pgid_of(child) == child,
            "child pgid is {}, expected {child}",
            task::process::pgid_of(child)
        );
        check!(
            task::process::sid_of(child) == root,
            "forming a group changed the session: {}",
            task::process::sid_of(child)
        );

        // Setting the group the target is already in is a successful no-op.
        task::process::setpgid(child, 0, 0).map_err(|error| format!("setpgid(0, 0): {error:?}"))?;
        check!(
            task::process::pgid_of(child) == child,
            "idempotent setpgid moved the child"
        );

        // A session leader cannot leave its group; a non-child cannot be moved;
        // a group outside the session does not exist; a negative pgid is EINVAL.
        check!(
            task::process::setpgid(root, root as i64, child as i64)
                == Err(GroupError::NotPermitted),
            "moved a session leader into another group"
        );
        check!(
            task::process::setpgid(child, root as i64, root as i64)
                == Err(GroupError::NotPermitted),
            "moved a process that is not the caller or its child"
        );
        check!(
            task::process::setpgid(root, child as i64, 9999) == Err(GroupError::NoSuchProcess),
            "joined a group that does not exist in the session"
        );
        check!(
            task::process::setpgid(child, 0, -1) == Err(GroupError::Invalid),
            "a negative pgid was accepted"
        );
        finish_and_reap_all(&chain)
    }

    /// `setsid` moves a non-leader into a fresh session and is `EPERM` for a
    /// group leader (so it cannot be called twice).
    pub fn setsid_new_session() -> Result<(), String> {
        let chain = fork_chain(2)?;
        task::harness::switch_current(task::KERNEL_TASK);
        let (root, child) = (chain[0], chain[1]);

        // Already a group leader: Linux returns EPERM and changes nothing.
        check!(
            task::process::setsid(root) == Err(GroupError::NotPermitted),
            "setsid succeeded for a group leader"
        );
        check!(
            task::process::sid_of(root) == root,
            "a failed setsid changed the sid"
        );

        check!(
            task::process::setsid(child) == Ok(child),
            "setsid did not return the new sid {child}"
        );
        check!(
            task::process::sid_of(child) == child,
            "child sid is {}, expected {child}",
            task::process::sid_of(child)
        );
        check!(
            task::process::pgid_of(child) == child,
            "child pgid is {}, expected {child}",
            task::process::pgid_of(child)
        );
        check!(
            task::process::sid_of(root) == root,
            "the parent session changed"
        );

        // The new leader cannot call setsid again.
        check!(
            task::process::setsid(child) == Err(GroupError::NotPermitted),
            "setsid succeeded twice"
        );
        finish_and_reap_all(&chain)
    }

    /// A dying task's children are adopted by the kernel/init task, and only
    /// the old parent can reap the corpse.
    pub fn reparent_on_death() -> Result<(), String> {
        let chain = fork_chain(3)?;
        task::harness::switch_current(task::KERNEL_TASK);
        let (root, child, leaf) = (chain[0], chain[1], chain[2]);
        check!(
            task::process::ppid_of(leaf) == child,
            "leaf ppid is {}, expected {child}",
            task::process::ppid_of(leaf)
        );

        check!(
            task::process::finish(child, 42),
            "finish(child) was a no-op"
        );
        check!(
            task::harness::state(child) == Some(task::TaskState::Done),
            "finished child is not Done"
        );
        check!(
            task::process::ppid_of(leaf) == 0,
            "orphan ppid is {}, expected init",
            task::process::ppid_of(leaf)
        );
        check!(
            task::process::children_of(task::KERNEL_TASK).contains(&leaf),
            "init's children do not include the orphan: {:?}",
            task::process::children_of(task::KERNEL_TASK)
        );

        // The live grandparent reaps the corpse, but not the adopted orphan:
        // that one is init's to collect.
        task::harness::switch_current(root);
        let (slot, status) = task::reap_child().ok_or("root could not reap its child")?;
        check!(
            slot == child && status == 42,
            "reaped slot {slot} with status {status}, expected {child}/42"
        );
        check!(
            task::reap_child().is_none(),
            "root reaped a task that is not its child"
        );
        task::harness::switch_current(task::KERNEL_TASK);

        check!(task::process::finish(leaf, 0), "finish(leaf) was a no-op");
        check!(task::process::finish(root, 0), "finish(root) was a no-op");
        let mut reaped = 0;
        while task::reap_child().is_some() {
            reaped += 1;
        }
        check!(reaped == 2, "init reaped {reaped} orphans, expected 2");
        task::harness::reset();
        Ok(())
    }

    /// `kill_group` marks every member `Done` (including blocked ones, which a
    /// later wake must not resurrect), spares init and other groups, and makes
    /// the corpses reapable by init.
    pub fn kill_group_terminates() -> Result<(), String> {
        let chain = fork_chain(3)?;
        task::harness::switch_current(task::KERNEL_TASK);
        let (root, child, leaf) = (chain[0], chain[1], chain[2]);

        // A sibling in its own group must survive the kill.
        task::harness::switch_current(root);
        let outsider = task::spawn_fork().map_err(|error| format!("outsider: {error}"))?;
        task::harness::switch_current(task::KERNEL_TASK);
        task::process::setpgid(root, outsider as i64, outsider as i64)
            .map_err(|error| format!("setpgid(outsider): {error:?}"))?;
        check!(
            task::process::pgid_of(outsider) == outsider,
            "outsider did not leave the group"
        );

        // Park a member first: a killed sleeper must stay Done (#57).
        let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
        queue.park(child, None);

        let killed = task::kill_group(root);
        check!(killed == 3, "kill_group killed {killed}, expected 3");
        for slot in [root, child, leaf] {
            check!(
                task::harness::state(slot) == Some(task::TaskState::Done),
                "group member {slot} survived: {:?}",
                task::harness::state(slot)
            );
        }
        check!(
            task::harness::state(outsider) == Some(task::TaskState::Runnable),
            "outsider was killed with the group"
        );
        check!(
            queue.notify_one() == 0,
            "a killed waiter was woken back to Runnable"
        );
        check!(
            task::harness::state(child) == Some(task::TaskState::Done),
            "a killed waiter was resurrected"
        );

        // init is exempt: only init terminates itself.
        check!(
            task::kill_group(task::KERNEL_TASK) == 0,
            "kill_group(0) killed init"
        );
        check!(
            task::harness::state(task::KERNEL_TASK) == Some(task::TaskState::Runnable),
            "init is no longer runnable"
        );

        // Every corpse was adopted by init: all four tasks are reapable there.
        task::harness::finish(outsider, 0);
        let mut reaped = 0;
        while task::reap_child().is_some() {
            reaped += 1;
        }
        check!(reaped == 4, "init reaped {reaped}, expected 4");
        task::harness::reset();
        Ok(())
    }

    /// `process_list` reports the kernel as pid 0 and every live task with its
    /// tree/group/session ids.
    pub fn process_list_snapshot() -> Result<(), String> {
        let chain = fork_chain(2)?;
        task::harness::switch_current(task::KERNEL_TASK);
        let (root, child) = (chain[0], chain[1]);

        let list = task::process::process_list();
        check!(
            list.len() == 3,
            "process_list has {} rows, expected 3 (init + 2)",
            list.len()
        );
        let init = list
            .iter()
            .find(|row| row.pid == 0)
            .ok_or("init is not listed")?;
        check!(
            init.slot == task::KERNEL_TASK
                && init.ppid == 0
                && init.pgid == 0
                && init.sid == 0
                && init.uid == 0
                && init.state == task::TaskState::Runnable
                && init.name == "kernel",
            "init row is {init:?}"
        );
        let row = list
            .iter()
            .find(|row| row.pid == child)
            .ok_or("forked child is not listed")?;
        check!(
            row.slot == child
                && row.ppid == root
                && row.pgid == root
                && row.sid == root
                && row.state == task::TaskState::Runnable
                && row.name == "fork",
            "child row is {row:?}"
        );
        finish_and_reap_all(&chain)
    }
}

// ---------------------------------------------------------------------------
// Signals (issue #60)
// ---------------------------------------------------------------------------

mod signal_suite {
    use super::*;
    use crate::task::signal::{self, Disposition, SigInfo};
    use crate::task::{TaskState, WaitKind, WakeReason};

    /// Each test starts from one runnable kernel task, an empty task table and
    /// an empty signal registry.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        signal::harness::reset();
        check!(
            task::harness::state(task::current()) == Some(TaskState::Runnable),
            "kernel task is not runnable after reset: {:?}",
            task::harness::state(task::current())
        );
        Ok(())
    }

    fn send(target: usize, sig: u8) -> Result<(), String> {
        let me = task::current();
        signal::send_to_slot(me, target, sig, SigInfo::user(me, signal::SI_USER))
            .map_err(|error| format!("send {sig} to {target}: {error:?}"))
    }

    /// A blocked signal stays pending and only becomes actionable when the mask
    /// clears; `SIGKILL`/`SIGSTOP` can never enter the blocked set.
    pub fn block_unblock_pending() -> Result<(), String> {
        fresh()?;
        let me = task::current();
        signal::set_blocked(me, 1 << signal::SIGINT);
        send(me, signal::SIGINT)?;
        check!(
            signal::pending(me) & (1 << signal::SIGINT) != 0,
            "a blocked signal was not queued: {:#x}",
            signal::pending(me)
        );
        check!(
            signal::blocked(me) & (1 << signal::SIGINT) != 0,
            "SIGINT did not stay blocked"
        );

        // Ignored signals are not queued at all (except the SIGCHLD record).
        let some = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        signal::set_action(me, signal::SIGUSR1, Disposition::Ignore)
            .map_err(|error| format!("set_action: {error:?}"))?;
        send(me, signal::SIGUSR1)?;
        check!(
            signal::pending(me) & (1 << signal::SIGUSR1) == 0,
            "an ignored signal was queued"
        );

        // The mask filter drops the uncatchable bits.
        signal::set_blocked(me, u64::MAX);
        check!(
            signal::blocked(me) & (1 << signal::SIGKILL) == 0,
            "SIGKILL entered the blocked mask"
        );
        check!(
            signal::blocked(me) & (1 << signal::SIGSTOP) == 0,
            "SIGSTOP entered the blocked mask"
        );
        signal::set_blocked(me, 0);
        check!(
            signal::pending(me) & (1 << signal::SIGINT) != 0,
            "unblocking dropped the pending signal"
        );

        task::harness::finish(some, 0);
        check!(task::reap_child().is_some(), "child was not reapable");
        task::harness::reset();
        signal::harness::reset();
        Ok(())
    }

    /// A queued term signal wakes a parked sleeper with `Interrupted`; a
    /// `SIGKILL` ends the victim outright and no later wake resurrects it.
    pub fn kill_wakes_blocked() -> Result<(), String> {
        fresh()?;
        let me = task::current();
        let sleeper = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        let queue = task::wait::WaitQueue::new(WaitKind::Sleep);
        queue.park(sleeper, None);
        send(sleeper, signal::SIGTERM)?;
        check!(
            task::harness::state(sleeper) == Some(TaskState::Runnable),
            "SIGTERM did not wake the sleeper: {:?}",
            task::harness::state(sleeper)
        );
        check!(
            task::harness::take_wake_reason(sleeper) == Some(WakeReason::Interrupted),
            "sleeper wake reason is not Interrupted"
        );
        check!(
            signal::pending(sleeper) & (1 << signal::SIGTERM) != 0,
            "SIGTERM was not left pending"
        );

        let victim = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        queue.park(victim, None);
        send(victim, signal::SIGKILL)?;
        check!(
            task::harness::state(victim) == Some(TaskState::Done),
            "SIGKILL left the victim {:?}",
            task::harness::state(victim)
        );
        check!(
            queue.notify_all() == 0,
            "notify resurrected a killed waiter"
        );
        check!(
            task::harness::state(victim) == Some(TaskState::Done),
            "SIGKILL victim was resurrected"
        );

        task::harness::finish(sleeper, 0);
        let mut reaped = 0;
        while task::reap_child().is_some() {
            reaped += 1;
        }
        check!(reaped == 2, "init reaped {reaped} corpses, expected 2");
        task::harness::reset();
        signal::harness::reset();
        Ok(())
    }

    /// `rt_sigaction` refuses `SIGKILL`/`SIGSTOP`, accepts others and reports
    /// them back; `rt_sigprocmask` cannot block the uncatchable pair.
    pub fn sigkill_uncatchable() -> Result<(), String> {
        fresh()?;
        let me = task::current();
        let mut action = [0u64; 4];
        action[0] = 0x40_1000; // handler
        action[2] = 0x40_2000; // restorer
        let act = action.as_mut_ptr() as u64;

        let e = process::linux::dispatch_for_test(13, signal::SIGKILL as u64, act, 0);
        check!(
            e == (-22i64) as u64,
            "rt_sigaction(SIGKILL) returned {e:#x}"
        );
        let e = process::linux::dispatch_for_test(13, signal::SIGSTOP as u64, act, 0);
        check!(
            e == (-22i64) as u64,
            "rt_sigaction(SIGSTOP) returned {e:#x}"
        );
        check!(
            signal::action(me, signal::SIGKILL) == Disposition::Default,
            "a refused SIGKILL action changed the disposition"
        );

        // A regular signal installs, and querying returns the same action.
        let e = process::linux::dispatch_for_test(13, signal::SIGTERM as u64, act, 0);
        check!(e == 0, "rt_sigaction(SIGTERM) returned {e:#x}");
        let expected = Disposition::Handler {
            handler: 0x40_1000,
            flags: 0,
            restorer: 0x40_2000,
            mask: 0,
        };
        check!(
            signal::action(me, signal::SIGTERM) == expected,
            "installed action is {:?}",
            signal::action(me, signal::SIGTERM)
        );
        let mut old = [0u64; 4];
        let e = process::linux::dispatch_for_test(
            13,
            signal::SIGTERM as u64,
            0,
            old.as_mut_ptr() as u64,
        );
        check!(e == 0, "querying SIGTERM returned {e:#x}");
        check!(
            old == [0x40_1000, 0, 0x40_2000, 0],
            "reported action is {old:?}"
        );

        // SIGKILL/SIGSTOP bits are discarded by rt_sigprocmask.
        let mask: u64 = (1 << signal::SIGKILL) | (1 << signal::SIGSTOP) | (1 << signal::SIGTERM);
        let e = process::linux::dispatch_for_test(
            14,
            signal::SIG_BLOCK,
            core::ptr::addr_of!(mask) as u64,
            0,
        );
        check!(e == 0, "rt_sigprocmask returned {e:#x}");
        check!(
            signal::blocked(me) & (1 << signal::SIGKILL) == 0
                && signal::blocked(me) & (1 << signal::SIGSTOP) == 0,
            "rt_sigprocmask blocked an uncatchable signal: {:#x}",
            signal::blocked(me)
        );
        check!(
            signal::blocked(me) & (1 << signal::SIGTERM) != 0,
            "rt_sigprocmask did not block SIGTERM"
        );
        signal::set_blocked(me, 0);
        signal::harness::reset();
        Ok(())
    }

    /// A child's exit leaves `SIGCHLD` pending on its parent (even under the
    /// default ignore disposition) while `wait4` still reaps it.
    pub fn sigchld_on_child_exit() -> Result<(), String> {
        fresh()?;
        let root = task::spawn_fork().map_err(|error| format!("spawn root: {error}"))?;
        task::harness::switch_current(root);
        let child = task::spawn_fork().map_err(|error| format!("spawn child: {error}"))?;
        check!(
            signal::pending(root) & (1 << signal::SIGCHLD) == 0,
            "SIGCHLD was pending before any exit"
        );
        task::harness::finish(child, 7);
        check!(
            signal::pending(root) & (1 << signal::SIGCHLD) != 0,
            "child exit did not post SIGCHLD: {:#x}",
            signal::pending(root)
        );
        let (slot, status) = task::reap_child().ok_or("parent could not reap its child")?;
        check!(
            slot == child && status == 7,
            "reaped {slot}/{status}, expected {child}/7"
        );

        task::harness::switch_current(task::KERNEL_TASK);
        task::harness::finish(root, 0);
        check!(task::reap_child().is_some(), "root was not reapable");
        task::harness::reset();
        signal::harness::reset();
        Ok(())
    }

    /// The Linux `rt_sigframe` layout round-trips: the handler's `pretcode`,
    /// `siginfo_t` and `ucontext_t` read back as written, and the native frame
    /// parser recovers the interrupted registers.
    pub fn handler_frame_roundtrip() -> Result<(), String> {
        fresh()?;
        let mut stack = alloc::vec![0u8; 8192];
        let top = stack.as_mut_ptr() as u64 + stack.len() as u64;
        let regs = signal::UserRegs {
            r15: 0x1515,
            r14: 0x1414,
            r13: 0x1313,
            r12: 0x1212,
            r11: 0x1111,
            r10: 0x1010,
            r9: 0x0909,
            r8: 0x0808,
            rbp: 0xb0b0,
            rdi: 0xd1d1,
            rsi: 0x5151,
            rdx: 0xd2d2,
            rcx: 0xc0c0,
            rbx: 0xb0b1,
            rax: 0xa0a0,
            rip: 0x0040_1000,
            rsp: top - 0x80,
            rflags: 0x202,
        };
        let info = SigInfo::fault(signal::SEGV_ACCERR, 0xdead_beef);
        let result = signal::build_linux_frame(
            top,
            &regs,
            signal::SIGSEGV,
            0x0040_2000,
            signal::SA_SIGINFO,
            0x0040_3000,
            0x2,
            0x0f,
            &info,
        );
        check!(
            result.rip == 0x0040_2000,
            "handler rip is {:#x}",
            result.rip
        );
        check!(result.rsp % 16 == 0, "frame is not 16-byte aligned");
        let pretcode = unsafe { core::ptr::read_volatile(result.rsp as *const u64) };
        check!(pretcode == 0x0040_3000, "pretcode is {pretcode:#x}");
        let signo = unsafe { core::ptr::read_volatile(result.info as *const i32) };
        check!(signo == signal::SIGSEGV as i32, "siginfo signo is {signo}");
        let (restored, mask) = signal::parse_linux_frame(result.rsp + 8);
        check!(mask == 0x0f, "saved mask is {mask:#x}");
        check!(restored == regs, "restored registers differ: {restored:?}");

        let native = signal::build_native_frame(top, &regs, signal::SIGTERM);
        let (native_regs, native_sig) = signal::parse_native_frame(native.rsp);
        check!(
            native_sig == signal::SIGTERM,
            "native frame signal is {native_sig}"
        );
        check!(native_regs == regs, "native frame lost registers");
        signal::harness::reset();
        Ok(())
    }

    /// A stop signal parks the whole process and only `SIGCONT` resumes it.
    pub fn stop_continue() -> Result<(), String> {
        fresh()?;
        let me = task::current();
        let child = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        send(child, signal::SIGSTOP)?;
        check!(
            task::harness::state(child)
                == Some(TaskState::Blocked {
                    wait: WaitKind::Signal,
                    deadline: None
                }),
            "SIGSTOP did not park the child: {:?}",
            task::harness::state(child)
        );
        send(child, signal::SIGCONT)?;
        check!(
            task::harness::state(child) == Some(TaskState::Runnable),
            "SIGCONT did not resume the child: {:?}",
            task::harness::state(child)
        );
        task::harness::finish(child, 0);
        check!(task::reap_child().is_some(), "child was not reapable");
        task::harness::reset();
        signal::harness::reset();
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
// Messenger channels and transactions (#66)
// ---------------------------------------------------------------------------

mod ipc_channel_suite {
    use super::*;
    use crate::ipc::channels::{self, Error as ChannelError};
    use crate::ipc::handles;
    use crate::task::{TaskState, WaitKind, WakeReason};
    use libmessenger::{flags, Decoder, Encoder, Header, Kind, Parcel, VERSION};

    /// Each channel test starts from an empty task table, an empty handle
    /// table, an empty channel registry, and a runnable kernel task with no
    /// stale wake reason.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        task::harness::reset();
        handles::reset_for_task(task::current());
        channels::reset();
        let me = task::current();
        let _ = task::harness::take_wake_reason(me);
        check!(
            task::harness::state(me) == Some(TaskState::Runnable),
            "kernel task is not runnable after reset: {:?}",
            task::harness::state(me)
        );
        Ok(())
    }

    /// Friendly-message adapter for `Result` plumbing.
    fn reason(error: ChannelError) -> String {
        error.message().into()
    }

    /// Encode a complete parcel whose body carries one string field.
    fn parcel(method: u32, parcel_flags: u16, text: &str) -> Result<Vec<u8>, String> {
        let mut body = Encoder::new();
        body.string(1, text).map_err(|error| error.message())?;
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: parcel_flags,
                interface_id: 0x1a2b_3c4d,
                method,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut bytes = Vec::new();
        parcel.encode(&mut bytes).map_err(|error| error.message())?;
        Ok(bytes)
    }

    /// Decode the first string field of a parcel body.
    fn payload(bytes: &[u8]) -> Result<String, String> {
        let parcel = Parcel::decode(bytes).map_err(|error| error.message())?;
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(|error| error.message())? {
            if field.kind == Kind::String {
                return Ok(field.as_str().map_err(|error| error.message())?.into());
            }
        }
        Err("parcel body has no string field".into())
    }

    fn blocked_call(slot: usize, deadline: Option<u64>) -> bool {
        matches!(
            task::harness::state(slot),
            Some(TaskState::Blocked {
                wait: WaitKind::Sleep,
                deadline: expected,
            }) if expected == deadline
        )
    }

    /// A synchronous call: the caller parks, the request keeps its bytes, the
    /// reply wakes the caller, and the round trip returns the reply parcel.
    pub fn echo_roundtrip() -> Result<(), String> {
        fresh()?;
        let (client, server) = channels::create().map_err(reason)?;
        let request = parcel(7, flags::SYNC, "ping")?;
        let start = unsafe { core::arch::x86_64::_rdtsc() };

        let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
        let me = task::current();
        check!(
            blocked_call(me, None),
            "begin_call did not park the caller: {:?}",
            task::harness::state(me)
        );

        // The server side sees the request with its kernel metadata intact.
        let message = channels::recv(server, None).map_err(reason)?;
        check!(
            message.sender == me,
            "sender is {}, expected {me}",
            message.sender
        );
        check!(message.method == 7, "method is {}", message.method);
        check!(
            message.txn == Some(txn),
            "transaction id is {:?}",
            message.txn
        );
        check!(message.bytes == request, "request bytes changed in flight");
        check!(message.handles.is_empty(), "request transferred handles");
        check!(
            payload(&message.bytes)? == "ping",
            "request payload changed"
        );

        // The reply is a fresh parcel, matched by transaction id.
        let reply = parcel(8, 0, "pong")?;
        channels::reply(txn, &reply).map_err(reason)?;
        check!(
            task::harness::state(me) == Some(TaskState::Runnable),
            "reply did not wake the caller: {:?}",
            task::harness::state(me)
        );
        check!(
            task::harness::take_wake_reason(me) == Some(WakeReason::Woken),
            "reply wake reason is not Woken"
        );
        let got = channels::await_reply(txn).map_err(reason)?;
        check!(got == reply, "reply bytes changed on the way back");
        check!(payload(&got)? == "pong", "reply payload changed");

        let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
        serial_println!("TEST:ipc_channel_echo_roundtrip:INFO:cycles={cycles}");
        let stats = channels::stats();
        check!(
            stats.calls == 1 && stats.replies == 1 && stats.timeouts == 0,
            "counters after one echo: {stats:?}"
        );
        check!(
            stats.queued == 0 && stats.queued_bytes == 0 && stats.outstanding == 0,
            "channel not drained: {stats:?}"
        );
        let senders = channels::senders(client).map_err(reason)?;
        check!(
            senders.len() == 1
                && senders[0].slot == me
                && senders[0].calls == 1
                && senders[0].sent == 1
                && senders[0].outstanding == 0,
            "sender metering is {senders:?}"
        );
        fresh()
    }

    /// One-way sends enqueue in order, never park the sender, and are refused
    /// with a metered drop when the peer's bounded queue is full.
    pub fn one_way_order_and_limits() -> Result<(), String> {
        fresh()?;
        let (client, server) = channels::create().map_err(reason)?;
        for index in 0..3u32 {
            let bytes = parcel(index, flags::ONE_WAY, &format!("m{index}"))?;
            channels::send(client, &bytes).map_err(reason)?;
        }
        let me = task::current();
        check!(
            task::harness::state(me) == Some(TaskState::Runnable),
            "send parked the sender: {:?}",
            task::harness::state(me)
        );
        for index in 0..3u32 {
            let message = channels::try_recv(server)
                .map_err(reason)?
                .ok_or("queued one-way message is missing")?;
            check!(
                message.txn.is_none(),
                "one-way message carries a transaction: {:?}",
                message.txn
            );
            check!(
                message.method == index,
                "order broken: method {}",
                message.method
            );
            check!(
                payload(&message.bytes)? == format!("m{index}"),
                "payload order broken"
            );
        }
        check!(
            channels::try_recv(server).map_err(reason)?.is_none(),
            "recv did not drain the queue"
        );

        // Fill the bounded queue, then observe the refusal and the drop meter.
        let bytes = parcel(0, flags::ONE_WAY, "fill")?;
        for _ in 0..channels::MAX_QUEUE_DEPTH {
            channels::send(client, &bytes).map_err(reason)?;
        }
        check!(
            channels::send(client, &bytes) == Err(ChannelError::QueueFull),
            "an overfull queue accepted a message"
        );
        let stats = channels::channel_stats(client).map_err(reason)?;
        check!(
            stats.drops == 1,
            "queue-full drop was not counted: {stats:?}"
        );
        check!(
            stats.queued == channels::MAX_QUEUE_DEPTH as u64,
            "queued depth is {}",
            stats.queued
        );
        let senders = channels::senders(server).map_err(reason)?;
        check!(
            senders.len() == 1 && senders[0].sent == 3 + channels::MAX_QUEUE_DEPTH as u64,
            "sender metering is {senders:?}"
        );
        fresh()
    }

    /// A call past its deadline wakes with `TimedOut`, and a late reply is
    /// refused rather than delivered.
    pub fn deadline_timeout() -> Result<(), String> {
        fresh()?;
        let (client, server) = channels::create().map_err(reason)?;
        let request = parcel(7, flags::SYNC, "slow")?;
        let me = task::current();
        let deadline = task::ticks() + 10;
        let txn = channels::begin_call(client, 7, &request, Some(deadline)).map_err(reason)?;
        check!(
            blocked_call(me, Some(deadline)),
            "caller did not park with its deadline: {:?}",
            task::harness::state(me)
        );

        channels::expire_deadlines(deadline);
        check!(
            task::harness::state(me) == Some(TaskState::Runnable),
            "deadline sweep did not wake the caller: {:?}",
            task::harness::state(me)
        );
        check!(
            task::harness::take_wake_reason(me) == Some(WakeReason::TimedOut),
            "deadline wake reason is not TimedOut"
        );
        let late = parcel(8, 0, "too late")?;
        check!(
            channels::reply(txn, &late) == Err(ChannelError::NoTransaction),
            "a reply to an expired transaction was accepted"
        );
        check!(
            channels::await_reply(txn) == Err(ChannelError::TimedOut),
            "await_reply did not report TimedOut"
        );
        let stats = channels::stats();
        check!(
            stats.timeouts == 1 && stats.outstanding == 0,
            "counters after a timeout: {stats:?}"
        );
        // The request stays queued for the (late) server to drain.
        check!(
            channels::try_recv(server).map_err(reason)?.is_some(),
            "the expired request vanished from the server queue"
        );
        fresh()
    }

    /// A reply that lands before the deadline sweep wins the race: the
    /// transaction completes normally and the timeout meter stays at zero.
    pub fn deadline_reply_race() -> Result<(), String> {
        fresh()?;
        let (client, _server) = channels::create().map_err(reason)?;
        let request = parcel(7, flags::SYNC, "fast")?;
        let deadline = task::ticks() + 10;
        let txn = channels::begin_call(client, 7, &request, Some(deadline)).map_err(reason)?;
        let reply = parcel(8, 0, "quick")?;
        channels::reply(txn, &reply).map_err(reason)?;
        // The sweep runs after the reply; it must not overwrite the outcome.
        channels::expire_deadlines(deadline);
        let got = channels::await_reply(txn).map_err(reason)?;
        check!(got == reply, "the racing reply was not returned");
        check!(
            channels::stats().timeouts == 0,
            "a completed reply was counted as timed out"
        );
        fresh()
    }

    /// The production `call` path, end to end: with an already-expired
    /// deadline the caller parks through the timer gate, the deadline sweep
    /// wakes it, and `call` returns `TimedOut` without a server.
    pub fn call_deadline_zero() -> Result<(), String> {
        fresh()?;
        let (client, _server) = channels::create().map_err(reason)?;
        let request = parcel(7, flags::SYNC, "nobody home")?;
        let start = unsafe { core::arch::x86_64::_rdtsc() };
        let result = channels::call(client, 7, &request, Some(0));
        let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
        check!(
            result == Err(ChannelError::TimedOut),
            "an already-expired call returned {result:?}"
        );
        serial_println!("TEST:ipc_channel_call_deadline_zero:INFO:cycles={cycles}");
        let stats = channels::stats();
        check!(
            stats.timeouts == 1 && stats.outstanding == 0,
            "counters after a timeout: {stats:?}"
        );
        check!(
            task::harness::state(task::current()) == Some(TaskState::Runnable),
            "the caller stayed parked after call returned"
        );
        fresh()
    }

    /// `cancel` wakes a parked caller with `Canceled`, and the transaction is
    /// gone afterwards.
    pub fn cancel_wakes() -> Result<(), String> {
        fresh()?;
        let (client, _server) = channels::create().map_err(reason)?;
        let request = parcel(7, flags::SYNC, "wait")?;
        let me = task::current();
        let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
        check!(blocked_call(me, None), "caller not parked before cancel");
        channels::cancel(txn).map_err(reason)?;
        check!(
            task::harness::state(me) == Some(TaskState::Runnable),
            "cancel did not wake the caller: {:?}",
            task::harness::state(me)
        );
        check!(
            task::harness::take_wake_reason(me) == Some(WakeReason::Woken),
            "cancel wake reason is not Woken"
        );
        check!(
            channels::await_reply(txn) == Err(ChannelError::Canceled),
            "await_reply did not report Canceled"
        );
        check!(
            channels::cancel(txn) == Err(ChannelError::NoTransaction),
            "double cancel succeeded"
        );
        check!(
            channels::stats().cancels == 1,
            "cancel counter is {}",
            channels::stats().cancels
        );
        fresh()
    }

    /// Closing an endpoint wakes an outstanding caller with `PeerDied`, and the
    /// surviving side sees `PeerDied` once its inbox is empty.
    pub fn peer_died() -> Result<(), String> {
        fresh()?;
        let (client, server) = channels::create().map_err(reason)?;
        let request = parcel(7, flags::SYNC, "hello?")?;
        let me = task::current();
        let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
        channels::close_endpoint(server).map_err(reason)?;
        check!(
            task::harness::state(me) == Some(TaskState::Runnable),
            "close did not wake the caller: {:?}",
            task::harness::state(me)
        );
        check!(
            task::harness::take_wake_reason(me) == Some(WakeReason::Woken),
            "close wake reason is not Woken"
        );
        check!(
            channels::await_reply(txn) == Err(ChannelError::PeerDied),
            "await_reply did not report PeerDied"
        );
        check!(
            channels::recv(client, None) == Err(ChannelError::PeerDied),
            "recv did not report PeerDied after the peer closed"
        );
        check!(
            channels::stats().outstanding == 0,
            "transaction stayed outstanding after the peer died"
        );
        fresh()
    }

    /// A synchronous call while another transaction is open on the channel is
    /// a cycle and refused with `Deadlock`; `ALLOW_NESTED` opts out, and the
    /// channel is usable again once the first transaction ends.
    pub fn deadlock_refused() -> Result<(), String> {
        fresh()?;
        let (a, b) = channels::create().map_err(reason)?;
        let request = parcel(7, flags::SYNC, "outer")?;
        let outer = channels::begin_call(a, 7, &request, None).map_err(reason)?;

        check!(
            channels::begin_call(b, 7, &request, None) == Err(ChannelError::Deadlock),
            "a nested call cycle was not refused"
        );
        let nested_bytes = parcel(7, flags::SYNC | flags::ALLOW_NESTED, "nested")?;
        let nested = channels::begin_call(b, 7, &nested_bytes, None).map_err(reason)?;
        check!(nested != outer, "the nested call reused the outer id");

        channels::cancel(outer).map_err(reason)?;
        channels::cancel(nested).map_err(reason)?;
        check!(
            channels::await_reply(outer) == Err(ChannelError::Canceled),
            "outer outcome is not Canceled"
        );
        check!(
            channels::await_reply(nested) == Err(ChannelError::Canceled),
            "nested outcome is not Canceled"
        );

        // With the channel idle again, a plain call is allowed.
        let again = channels::begin_call(a, 7, &request, None).map_err(reason)?;
        channels::cancel(again).map_err(reason)?;
        check!(
            channels::await_reply(again) == Err(ChannelError::Canceled),
            "reused channel outcome is not Canceled"
        );
        check!(
            channels::stats().cancels == 3,
            "cancel counter is {}",
            channels::stats().cancels
        );
        fresh()
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
