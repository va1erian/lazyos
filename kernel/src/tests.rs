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
use alloc::vec;
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
    (
        "slab_alloc_distinct_aligned",
        slab_suite::alloc_distinct_aligned,
    ),
    ("slab_reuse_after_free", slab_suite::reuse_after_free),
    ("slab_stats_live_peak", slab_suite::stats_live_peak),
    ("slab_oversized_fallback", slab_suite::oversized_fallback),
    ("slab_owner_accounting", slab_suite::owner_accounting),
    ("slab_soak_bounded_live", slab_suite::soak_bounded_live),
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
        "task_sched_strict_classes_no_starvation",
        sched_suite::strict_classes_no_starvation,
    ),
    (
        "task_sched_weighted_share_within_class",
        sched_suite::weighted_share_within_class,
    ),
    (
        "task_sched_skips_blocked_and_done",
        sched_suite::skips_blocked_and_done,
    ),
    (
        "task_sched_priority_api_and_cpu_accounting",
        sched_suite::priority_api_and_cpu_accounting,
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
    (
        "ipc_channel_begin_call_wakes_callee",
        ipc_channel_suite::begin_call_wakes_callee,
    ),
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
    (
        "ipc_buffer_create_write_read",
        ipc_shared_suite::buffer_create_write_read,
    ),
    ("ipc_buffer_quota", ipc_shared_suite::buffer_quota),
    (
        "ipc_buffer_share_only_not_mappable",
        ipc_shared_suite::buffer_share_only_not_mappable,
    ),
    (
        "ipc_buffer_handle_transfer_rights",
        ipc_shared_suite::buffer_handle_transfer_rights,
    ),
    (
        "ipc_buffer_fence_submit_wait",
        ipc_shared_suite::buffer_fence_submit_wait,
    ),
    (
        "ipc_buffer_zero_copy_handoff",
        ipc_shared_suite::buffer_zero_copy_handoff,
    ),
    ("ipc_messenger_syscall_echo", messenger_suite::syscall_echo),
    (
        "ipc_messenger_syscall_timeout",
        messenger_suite::syscall_timeout,
    ),
    (
        "ipc_messenger_syscall_denied",
        messenger_suite::syscall_denied,
    ),
    (
        "ipc_messenger_syscall_bad_pointer",
        messenger_suite::syscall_bad_pointer,
    ),
    (
        "ipc_messenger_bootstrap_claim",
        messenger_suite::bootstrap_claim,
    ),
    (
        "ipc_registry_register_resolve_roundtrip",
        registry_suite::register_resolve_roundtrip,
    ),
    (
        "ipc_registry_unknown_name_friendly",
        registry_suite::unknown_name_friendly,
    ),
    (
        "ipc_registry_lease_expiry_prunes",
        registry_suite::lease_expiry_prunes,
    ),
    (
        "ipc_registry_owner_death_releases",
        registry_suite::owner_death_releases,
    ),
    (
        "ipc_registry_acl_denies_register",
        registry_suite::acl_denies_register,
    ),
    (
        "ipc_registry_list_reflects_state",
        registry_suite::list_reflects_state,
    ),
    (
        "ipc_registry_proxy_registers_for_client",
        registry_suite::proxy_registers_for_client,
    ),
    (
        "ipc_registry_syscall_roundtrip",
        registry_suite::syscall_roundtrip,
    ),
    (
        "ipc_messenger_fabric_stats_abi",
        messenger_suite::syscall_fabric_stats,
    ),
    (
        "ipc_stats_snapshot_reflects_objects",
        stats_suite::snapshot_reflects_objects,
    ),
    (
        "ipc_stats_reset_restores_zeros",
        stats_suite::reset_restores_zeros,
    ),
    (
        "ipc_stats_counters_follow_traffic",
        stats_suite::counters_follow_calls_and_denials,
    ),
    (
        "ipc_topic_segment_methods_stable",
        topics_suite::segment_methods_stable,
    ),
    (
        "ipc_topic_acl_segments_enforced",
        topics_suite::acl_segments_enforced,
    ),
    (
        "ipc_topic_acl_wildcard_filter",
        topics_suite::acl_wildcard_filter,
    ),
    ("ipc_topic_acl_syscall_gate", topics_suite::syscall_gate),
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
// Slab allocator (issue #61)
// ---------------------------------------------------------------------------

mod slab_suite {
    use super::*;
    use crate::mem::slab;
    use core::ptr::NonNull;

    /// Every class hands out distinct, class-aligned, zeroed slots whose full
    /// payload round-trips, and the counters return to baseline after freeing.
    pub fn alloc_distinct_aligned() -> Result<(), String> {
        let baseline = slab::stats();
        let mut held: Vec<(usize, NonNull<u8>)> = Vec::new();
        for class in 0..slab::CLASS_COUNT {
            let size = slab::class_size(class);
            for index in 0..3usize {
                let ptr = slab::alloc(class)
                    .ok_or_else(|| format!("class {class} slot {index}: alloc returned None"))?;
                let address = ptr.as_ptr() as usize;
                check!(
                    address % size == 0,
                    "class {class} slot {index} at {address:#x} is not {size}-aligned"
                );
                check!(
                    !held
                        .iter()
                        .any(|(_, other)| other.as_ptr() as usize == address),
                    "class {class} slot {index} at {address:#x} was handed out twice"
                );
                // Slots are zeroed on allocation.
                for offset in [0, size / 2, size - 1] {
                    // Safety: the slot is ours and `offset` is inside it.
                    let byte = unsafe { ptr.as_ptr().add(offset).read_volatile() };
                    check!(
                        byte == 0,
                        "class {class} slot {index} byte {offset} is {byte:#x}, not zeroed"
                    );
                }
                // Fill and verify the whole payload (exactly the class size).
                let seed = (class as u8) ^ (index as u8).wrapping_mul(31);
                for offset in 0..size {
                    // Safety: as above.
                    unsafe { ptr.as_ptr().add(offset).write_volatile(seed ^ offset as u8) };
                }
                for offset in 0..size {
                    // Safety: as above.
                    let got = unsafe { ptr.as_ptr().add(offset).read_volatile() };
                    check!(
                        got == seed ^ offset as u8,
                        "class {class} slot {index} corrupted at {offset}: {got:#x}"
                    );
                }
                held.push((class, ptr));
            }
        }

        let during = slab::stats();
        let expected: usize = (0..slab::CLASS_COUNT)
            .map(|class| 3 * slab::class_size(class))
            .sum();
        check!(
            during.live_bytes == baseline.live_bytes + expected,
            "live bytes while holding all slots: {} (expected {})",
            during.live_bytes,
            baseline.live_bytes + expected
        );
        check!(
            during.peak_bytes >= during.live_bytes,
            "peak {} is below live {}",
            during.peak_bytes,
            during.live_bytes
        );
        for class in 0..slab::CLASS_COUNT {
            check!(
                during.classes[class].live == baseline.classes[class].live + 3,
                "class {class} live is {}, expected {}",
                during.classes[class].live,
                baseline.classes[class].live + 3
            );
        }

        for (class, ptr) in held.into_iter().rev() {
            // Safety: each slot is live and freed exactly once, with its class.
            unsafe { slab::dealloc(class, ptr) };
        }
        let after = slab::stats();
        check!(
            after.live_bytes == baseline.live_bytes,
            "live bytes after freeing all slots: {}, baseline {}",
            after.live_bytes,
            baseline.live_bytes
        );
        for class in 0..slab::CLASS_COUNT {
            check!(
                after.classes[class].live == baseline.classes[class].live,
                "class {class} live after freeing: {}, baseline {}",
                after.classes[class].live,
                baseline.classes[class].live
            );
            check!(
                after.classes[class].frees == baseline.classes[class].frees + 3,
                "class {class} did not record three frees"
            );
        }
        Ok(())
    }

    /// The free list is LIFO: freeing slots and allocating again returns the
    /// most recently freed slot first, for every class.
    pub fn reuse_after_free() -> Result<(), String> {
        let baseline = slab::stats();
        for class in 0..slab::CLASS_COUNT {
            let first = slab::alloc(class).ok_or_else(|| format!("class {class}: alloc failed"))?;
            let second =
                slab::alloc(class).ok_or_else(|| format!("class {class}: alloc failed"))?;
            check!(
                first != second,
                "class {class}: two live slots share an address"
            );

            // Free second then first: first is on top and must come back.
            // Safety: both slots are live and freed exactly once.
            unsafe {
                slab::dealloc(class, second);
                slab::dealloc(class, first);
            }
            let reused =
                slab::alloc(class).ok_or_else(|| format!("class {class}: realloc failed"))?;
            check!(
                reused == first,
                "class {class}: free list handed back {:#x}, expected {:#x}",
                reused.as_ptr() as usize,
                first.as_ptr() as usize
            );
            // Safety: as above.
            unsafe { slab::dealloc(class, reused) };
        }
        let after = slab::stats();
        check!(
            after.live_bytes == baseline.live_bytes,
            "reuse test left {} live bytes over baseline",
            after.live_bytes.saturating_sub(baseline.live_bytes)
        );
        for class in 0..slab::CLASS_COUNT {
            check!(
                after.classes[class].live == baseline.classes[class].live,
                "class {class} live is {}, baseline {}",
                after.classes[class].live,
                baseline.classes[class].live
            );
        }
        Ok(())
    }

    /// `class_for_size` rounds up at the class boundaries, `stats` tracks
    /// live/peak per class, and peak survives the frees.
    pub fn stats_live_peak() -> Result<(), String> {
        check!(
            slab::CLASSES == [32, 64, 128, 256, 512, 1024, 2048, 4096],
            "size classes changed: {:?}",
            slab::CLASSES
        );
        check!(
            slab::class_for_size(0) == Some(0),
            "size 0 did not pick a class"
        );
        check!(
            slab::class_for_size(1) == Some(0),
            "size 1 did not pick class 0"
        );
        check!(
            slab::class_for_size(32) == Some(0),
            "size 32 did not pick class 0"
        );
        check!(
            slab::class_for_size(33) == Some(1),
            "size 33 did not round up to class 1"
        );
        check!(
            slab::class_for_size(4096) == Some(7),
            "the largest size did not pick the last class"
        );
        check!(
            slab::class_for_size(4097).is_none(),
            "a size above the classes picked a slab class"
        );
        check!(
            slab::alloc(slab::CLASS_COUNT).is_none(),
            "alloc accepted an out-of-range class"
        );

        let baseline = slab::stats();
        let small = 2; // 128-byte slots
        let large = 4; // 512-byte slots
        let mut held: Vec<(usize, NonNull<u8>)> = Vec::new();
        for _ in 0..4 {
            held.push((
                small,
                slab::alloc(small).ok_or("128-byte class alloc failed")?,
            ));
        }
        for _ in 0..2 {
            held.push((
                large,
                slab::alloc(large).ok_or("512-byte class alloc failed")?,
            ));
        }

        let during = slab::stats();
        let bytes = 4 * slab::class_size(small) + 2 * slab::class_size(large);
        check!(
            during.live_bytes == baseline.live_bytes + bytes,
            "live bytes are {}, expected {}",
            during.live_bytes,
            baseline.live_bytes + bytes
        );
        check!(
            during.peak_bytes >= during.live_bytes,
            "peak {} is below live {}",
            during.peak_bytes,
            during.live_bytes
        );
        check!(
            during.classes[small].live == baseline.classes[small].live + 4,
            "small class live is {}, expected {}",
            during.classes[small].live,
            baseline.classes[small].live + 4
        );
        check!(
            during.classes[large].live == baseline.classes[large].live + 2,
            "large class live is {}, expected {}",
            during.classes[large].live,
            baseline.classes[large].live + 2
        );
        check!(
            during.classes[small].peak >= during.classes[small].live,
            "class peak below live"
        );

        for (class, ptr) in held {
            // Safety: each slot is live and freed exactly once.
            unsafe { slab::dealloc(class, ptr) };
        }
        let after = slab::stats();
        check!(
            after.live_bytes == baseline.live_bytes,
            "live bytes after frees: {}, baseline {}",
            after.live_bytes,
            baseline.live_bytes
        );
        check!(
            after.classes[small].live == baseline.classes[small].live
                && after.classes[large].live == baseline.classes[large].live,
            "class live did not return to baseline"
        );
        check!(
            after.peak_bytes >= during.live_bytes,
            "peak forgot the high-water mark"
        );
        Ok(())
    }

    /// Requests above the largest class use the heap fallback, are zeroed and
    /// writable end to end, and are reported by the oversized counters.
    pub fn oversized_fallback() -> Result<(), String> {
        let baseline = slab::stats();
        let size = slab::MAX_SLAB_SIZE + 123;
        check!(
            slab::class_for_size(size).is_none(),
            "an oversized request picked a slab class"
        );
        let ptr = slab::alloc_bytes(size).ok_or("oversized alloc_bytes returned None")?;
        check!(
            ptr.as_ptr() as usize % 16 == 0,
            "oversized allocation {:#x} is not 16-aligned",
            ptr.as_ptr() as usize
        );
        for offset in (0..size).step_by(64) {
            // Safety: the whole allocation is ours.
            let byte = unsafe { ptr.as_ptr().add(offset).read_volatile() };
            check!(
                byte == 0,
                "oversized byte {offset} is {byte:#x}, not zeroed"
            );
        }
        for offset in [0, size / 2, size - 1] {
            // Safety: as above.
            unsafe { ptr.as_ptr().add(offset).write_volatile(0xa5) };
        }
        for offset in [0, size / 2, size - 1] {
            // Safety: as above.
            let got = unsafe { ptr.as_ptr().add(offset).read_volatile() };
            check!(got == 0xa5, "oversized byte {offset} is {got:#x}");
        }

        let during = slab::stats();
        check!(
            during.oversized_allocations == baseline.oversized_allocations + 1,
            "oversized allocation was not counted"
        );
        check!(
            during.oversized_bytes == baseline.oversized_bytes + size,
            "oversized live bytes are {}, expected {}",
            during.oversized_bytes,
            baseline.oversized_bytes + size
        );
        check!(
            during.oversized_peak_bytes >= during.oversized_bytes,
            "oversized peak below live"
        );

        // Safety: the allocation is live and freed once with the same size.
        unsafe { slab::dealloc_bytes(size, ptr) };
        let after = slab::stats();
        check!(
            after.oversized_bytes == baseline.oversized_bytes,
            "oversized live bytes after free: {}, baseline {}",
            after.oversized_bytes,
            baseline.oversized_bytes
        );
        check!(
            after.oversized_frees == baseline.oversized_frees + 1,
            "oversized free was not counted"
        );

        // An impossible layout is refused, not a panic.
        check!(
            slab::alloc_bytes(usize::MAX / 2).is_none(),
            "an impossible allocation size was accepted"
        );
        Ok(())
    }

    /// `charge`/`uncharge` are owner-scoped, record peaks, count errors, and
    /// saturate instead of underflowing or panicking.
    pub fn owner_accounting() -> Result<(), String> {
        let owner = slab::MAX_OWNERS - 1;
        let other = slab::MAX_OWNERS - 2;
        let baseline = slab::owner_stats(owner).ok_or("owner slot is invalid")?;
        let other_baseline = slab::owner_stats(other).ok_or("second owner slot is invalid")?;

        check!(slab::charge(owner, 100), "charge(100) failed");
        check!(slab::charge(owner, 200), "charge(200) failed");
        let during = slab::owner_stats(owner).ok_or("owner vanished")?;
        check!(
            during.live_bytes == baseline.live_bytes + 300,
            "owner live is {}, expected {}",
            during.live_bytes,
            baseline.live_bytes + 300
        );
        check!(
            during.peak_bytes >= during.live_bytes,
            "owner peak below live"
        );
        check!(
            during.charges == baseline.charges + 2,
            "owner charges are {}, expected {}",
            during.charges,
            baseline.charges + 2
        );
        check!(
            slab::owner_stats(other).map(|state| state.live_bytes)
                == Some(other_baseline.live_bytes),
            "charging one owner moved another"
        );

        check!(slab::uncharge(owner, 100), "uncharge(100) failed");
        let during = slab::owner_stats(owner).ok_or("owner vanished")?;
        check!(
            during.live_bytes == baseline.live_bytes + 200,
            "owner live is {}, expected {}",
            during.live_bytes,
            baseline.live_bytes + 200
        );

        // Over-uncharge is an error, saturates at zero, never wraps.
        let errors = slab::stats().accounting_errors;
        check!(!slab::uncharge(owner, 10_000), "an over-uncharge succeeded");
        let during = slab::owner_stats(owner).ok_or("owner vanished")?;
        check!(
            during.live_bytes == 0,
            "over-uncharge did not saturate at zero"
        );
        check!(
            slab::stats().accounting_errors == errors + 1,
            "over-uncharge was not counted"
        );

        // Out-of-range owners are refused and counted.
        let errors = slab::stats().accounting_errors;
        check!(
            !slab::charge(slab::MAX_OWNERS, 1),
            "charge accepted an out-of-range owner"
        );
        check!(
            !slab::uncharge(slab::MAX_OWNERS, 1),
            "uncharge accepted an out-of-range owner"
        );
        check!(
            slab::owner_stats(slab::MAX_OWNERS).is_none(),
            "bad owner has stats"
        );
        check!(
            slab::stats().accounting_errors == errors + 2,
            "bad-owner calls were not counted"
        );
        Ok(())
    }

    /// Soak: a million alloc/free rounds across all classes with a moving
    /// window of live slots. Live bytes must stay bounded by the window (no
    /// leak, no fragmentation blow-up) and drain back to baseline.
    pub fn soak_bounded_live() -> Result<(), String> {
        const ROUNDS: usize = 1_000_000;
        const WINDOW: usize = 8;

        let baseline = slab::stats();
        let ceiling = baseline.live_bytes + WINDOW * slab::MAX_SLAB_SIZE;
        let start = unsafe { core::arch::x86_64::_rdtsc() };
        let mut window: [Option<(usize, NonNull<u8>)>; WINDOW] = [None; WINDOW];

        for round in 0..ROUNDS {
            let class = (round / 3) % slab::CLASS_COUNT;
            let size = slab::class_size(class);
            let ptr = slab::alloc(class)
                .ok_or_else(|| format!("round {round}: class {class} alloc failed"))?;
            let seed = (round as u8).wrapping_mul(31);
            // Safety: the slot is ours and at least `size` bytes long.
            unsafe {
                ptr.as_ptr().write_volatile(seed);
                ptr.as_ptr().add(size - 1).write_volatile(seed ^ 0xff);
            }

            let slot = round % WINDOW;
            if let Some((old_class, old)) = window[slot].take() {
                // The slot allocated `WINDOW` rounds ago still holds its own
                // pattern: recycled slots do not bleed into each other.
                let old_seed = ((round - WINDOW) as u8).wrapping_mul(31);
                // Safety: `old` is still live and owned by this task.
                let first = unsafe { old.as_ptr().read_volatile() };
                let last = unsafe {
                    old.as_ptr()
                        .add(slab::class_size(old_class) - 1)
                        .read_volatile()
                };
                check!(
                    first == old_seed && last == old_seed ^ 0xff,
                    "round {round}: slot recycling corrupted data"
                );
                // Safety: `old` is freed exactly once.
                unsafe { slab::dealloc(old_class, old) };
            }
            window[slot] = Some((class, ptr));

            if round % (ROUNDS / 16) == 0 {
                let live = slab::stats().live_bytes;
                check!(
                    live <= ceiling,
                    "round {round}: live {live} bytes over the ceiling {ceiling}"
                );
            }
        }

        for (class, ptr) in window.into_iter().flatten() {
            // Safety: each remaining window slot is live and freed once.
            unsafe { slab::dealloc(class, ptr) };
        }
        let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
        let after = slab::stats();
        check!(
            after.live_bytes == baseline.live_bytes,
            "soak leaked {} live bytes and {} slots",
            after.live_bytes.saturating_sub(baseline.live_bytes),
            (0..slab::CLASS_COUNT)
                .map(|class| after.classes[class].live)
                .sum::<usize>()
        );
        for class in 0..slab::CLASS_COUNT {
            check!(
                after.classes[class].live == baseline.classes[class].live,
                "class {class} live is {}, baseline {}",
                after.classes[class].live,
                baseline.classes[class].live
            );
        }
        serial_println!(
            "TEST:slab_soak_bounded_live:INFO:rounds={ROUNDS} ops={} cycles={cycles} peak_bytes={}",
            ROUNDS * 2,
            after.peak_bytes.saturating_sub(baseline.live_bytes)
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
// Priority classes and fair-share scheduling (issue #58)
// ---------------------------------------------------------------------------

mod sched_suite {
    use super::*;
    use crate::task::{PriorityClass, TaskState, MAX_WEIGHT, MIN_WEIGHT};

    /// Fresh table: kernel registered (Interactive) with zeroed accounting.
    /// The kernel is parked so the simulations below exercise the user
    /// scheduler alone; [`cleanup`] wakes it again.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            task::priority(task::KERNEL_TASK) == Some(PriorityClass::Interactive),
            "the kernel mux is not Interactive: {:?}",
            task::priority(task::KERNEL_TASK)
        );
        task::set_blocked(true);
        check!(
            task::harness::state(task::KERNEL_TASK)
                == Some(TaskState::Blocked {
                    wait: task::WaitKind::Sleep,
                    deadline: None,
                }),
            "the kernel was not parked for the simulation: {:?}",
            task::harness::state(task::KERNEL_TASK)
        );
        Ok(())
    }

    /// Spawn a fork child (which inherits the kernel's class) and put it in
    /// `class`.
    fn child(class: PriorityClass) -> Result<usize, String> {
        let slot = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        check!(
            task::set_priority(slot, class),
            "set_priority({slot}) failed"
        );
        check!(
            task::priority(slot) == Some(class),
            "slot {slot} did not take class {}",
            class.label()
        );
        Ok(slot)
    }

    /// Simulate `ticks` timer decisions; returns the picked slot per tick and
    /// each task's charged CPU ticks.
    fn simulate(ticks: usize) -> (Vec<usize>, Vec<(usize, u64)>) {
        let mut picks = Vec::new();
        for _ in 0..ticks {
            picks.push(task::harness::simulate_tick());
        }
        let usage = task::cpu_usage()
            .into_iter()
            .map(|row| (row.slot, row.ticks))
            .collect();
        (picks, usage)
    }

    fn runs(picks: &[usize], slot: usize) -> usize {
        picks.iter().filter(|&&pick| pick == slot).count()
    }

    /// Count the ticks charged to `slot` in a [`simulate`] report.
    fn charged(usage: &[(usize, u64)], slot: usize) -> u64 {
        usage
            .iter()
            .find(|(index, _)| *index == slot)
            .map(|(_, ticks)| *ticks)
            .unwrap_or(0)
    }

    /// Finish every live task, wake the kernel and reset the table.
    fn cleanup() {
        for slot in 1..task::MAX_TASKS {
            if task::harness::state(slot).is_some() {
                task::harness::finish(slot, 0);
            }
        }
        task::harness::switch_current(task::KERNEL_TASK);
        while task::reap_child().is_some() {}
        task::set_blocked(false);
        task::harness::reset();
    }

    /// A `Background` CPU hog cannot starve an `Interactive` task: class beats
    /// weight, so the interactive task runs every tick even against the
    /// heaviest background hog. Once it blocks, the background task gets the
    /// CPU, proving it was only preempted, not starved.
    pub fn strict_classes_no_starvation() -> Result<(), String> {
        fresh()?;
        let interactive = child(PriorityClass::Interactive)?;
        let background = child(PriorityClass::Background)?;
        check!(
            task::set_weight(background, MAX_WEIGHT) && task::set_weight(interactive, MIN_WEIGHT),
            "set_weight failed on a live task"
        );
        check!(
            task::weight(background) == Some(MAX_WEIGHT)
                && task::weight(interactive) == Some(MIN_WEIGHT),
            "weights are {:?}/{:?}",
            task::weight(interactive),
            task::weight(background)
        );

        let ticks = 200usize;
        let (picks, usage) = simulate(ticks);
        check!(
            runs(&picks, interactive) == ticks,
            "Interactive ran {} of {ticks} ticks",
            runs(&picks, interactive)
        );
        check!(
            runs(&picks, background) == 0,
            "Background ran {} ticks while Interactive was runnable",
            runs(&picks, background)
        );
        check!(
            charged(&usage, interactive) >= ticks as u64 - 1 && charged(&usage, background) == 0,
            "CPU ticks leaked to the wrong class: {:?}",
            usage
        );

        // The background task is not starved forever: the moment the
        // interactive task blocks, it runs.
        let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
        queue.park(interactive, None);
        let (picks, _) = simulate(20);
        check!(
            runs(&picks, background) == 20,
            "Background inherited the CPU only {} of 20 ticks after Interactive blocked",
            runs(&picks, background)
        );
        cleanup();
        Ok(())
    }

    /// Within one class, weights set the share: a weight-4 and a weight-1
    /// `Normal` task split the simulated ticks 4:1 because their strides are
    /// inverse to their weights.
    pub fn weighted_share_within_class() -> Result<(), String> {
        fresh()?;
        let heavy = child(PriorityClass::Normal)?;
        let light = child(PriorityClass::Normal)?;
        check!(
            task::set_weight(heavy, 4) && task::set_weight(light, 1),
            "set_weight failed on a live task"
        );

        let ticks = 2500usize;
        task::harness::switch_current(heavy);
        let (picks, usage) = simulate(ticks);
        let heavy_runs = runs(&picks, heavy);
        let light_runs = runs(&picks, light);
        check!(
            heavy_runs + light_runs == ticks,
            "the class only used {}/{ticks} ticks",
            heavy_runs + light_runs
        );
        let expected = ticks * 4 / 5;
        check!(
            heavy_runs.abs_diff(expected) <= 2,
            "weight-4 task ran {heavy_runs} times, expected ~{expected}"
        );
        check!(
            light_runs.abs_diff(ticks - expected) <= 2,
            "weight-1 task ran {light_runs} times, expected ~{}",
            ticks - expected
        );
        // The same 4:1 split shows up in the CPU accounting (within one tick
        // of the pick counts, because the first tick charges the start task).
        let heavy_ticks = charged(&usage, heavy);
        let light_ticks = charged(&usage, light);
        check!(
            heavy_ticks + light_ticks == ticks as u64,
            "charged {}+{} ticks for {ticks} decisions",
            heavy_ticks,
            light_ticks
        );
        check!(
            heavy_ticks.abs_diff(heavy_runs as u64) <= 1
                && light_ticks.abs_diff(light_runs as u64) <= 1,
            "CPU accounting ({heavy_ticks}/{light_ticks}) disagrees with selections ({heavy_runs}/{light_runs})"
        );
        cleanup();
        Ok(())
    }

    /// Blocked and done tasks are never selected; the kernel is the pick only
    /// while no user task can run, and a wake puts the user task back first.
    pub fn skips_blocked_and_done() -> Result<(), String> {
        fresh()?;
        let interactive = child(PriorityClass::Interactive)?;
        let background = child(PriorityClass::Background)?;

        let queue = task::wait::WaitQueue::new(task::WaitKind::Sleep);
        queue.park(interactive, None);
        task::harness::finish(background, 0);
        check!(
            matches!(
                task::harness::state(interactive),
                Some(TaskState::Blocked { .. })
            ),
            "the interactive task is not blocked: {:?}",
            task::harness::state(interactive)
        );
        check!(
            task::harness::state(background) == Some(TaskState::Done),
            "the background task is not done"
        );
        // Wake the parked kernel: with both user tasks parked/done it is the
        // only candidate left.
        task::wake_task(task::KERNEL_TASK);
        check!(
            task::harness::next_runnable() == task::KERNEL_TASK,
            "the kernel was not chosen when every user task is parked/done"
        );

        let (picks, _) = simulate(10);
        check!(
            picks.iter().all(|&slot| slot == task::KERNEL_TASK),
            "a parked or done task was selected: {picks:?}"
        );

        // A wake makes the user task selectable again: with the mux parked
        // (as it is between frames) the woken task is the only candidate and
        // never loses a tick to a parked or done slot.
        task::set_blocked(true);
        check!(
            queue.notify_one() == 1,
            "notify_one did not wake the interactive task"
        );
        check!(
            task::harness::next_runnable() == interactive,
            "woken task {interactive} is not the next pick"
        );
        let (picks, _) = simulate(5);
        check!(
            picks.iter().all(|&slot| slot == interactive),
            "the woken task did not get the CPU: {picks:?}"
        );
        cleanup();
        Ok(())
    }

    /// The priority API and CPU accounting: defaults by kind, class changes
    /// reset the weight, `set_weight` clamps, and `cpu_usage` reports the
    /// ticks charged by the timer path.
    pub fn priority_api_and_cpu_accounting() -> Result<(), String> {
        fresh()?;
        let slot = child(PriorityClass::Normal)?;
        check!(
            task::weight(slot) == Some(PriorityClass::Normal.default_weight()),
            "a new Normal task has weight {:?}",
            task::weight(slot)
        );

        check!(
            task::set_weight(slot, u16::MAX),
            "set_weight failed on a live task"
        );
        check!(
            task::weight(slot) == Some(MAX_WEIGHT),
            "weight was not clamped: {:?}",
            task::weight(slot)
        );
        check!(
            task::set_priority(slot, PriorityClass::Realtime),
            "set_priority failed on a live task"
        );
        check!(
            task::weight(slot) == Some(PriorityClass::Realtime.default_weight()),
            "set_priority did not reset the weight: {:?}",
            task::weight(slot)
        );

        // Empty and out-of-range slots report None and reject writes.
        check!(
            task::priority(task::MAX_TASKS + 7).is_none() && task::priority(0).is_some(),
            "priority mishandled an invalid or empty slot"
        );
        check!(
            !task::set_priority(task::MAX_TASKS + 7, PriorityClass::Normal)
                && !task::set_weight(task::MAX_TASKS + 7, 1),
            "the priority API accepted an invalid slot"
        );

        // Simulate ticks with the child as the only runnable user task: it is
        // selected throughout and the charged ticks land in its row.
        task::harness::switch_current(slot);
        let ticks = 100usize;
        let (picks, usage) = simulate(ticks);
        check!(
            picks.iter().all(|&pick| pick == slot),
            "the only runnable user task was not selected: {picks:?}"
        );
        let charged_total: u64 = usage.iter().map(|(_, ticks)| *ticks).sum();
        check!(
            charged_total == ticks as u64,
            "charged {charged_total} ticks for {ticks} decisions: {usage:?}"
        );
        check!(
            task::cpu_ticks(slot) == charged(&usage, slot)
                && task::cpu_ticks(task::KERNEL_TASK) == 0,
            "cpu_ticks disagrees with cpu_usage: {:?}",
            usage
        );

        let row = task::cpu_usage()
            .into_iter()
            .find(|row| row.slot == slot)
            .ok_or("the child is missing from cpu_usage")?;
        check!(
            row.class == PriorityClass::Realtime
                && row.name == "fork"
                && row.state == TaskState::Runnable
                && row.ticks == charged(&usage, slot),
            "cpu_usage row is {row:?}"
        );
        cleanup();
        Ok(())
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

    /// A callee already parked in `recv` is woken by `begin_call`: the request
    /// enqueue notifies the messenger queue, exactly as `send` does. Without
    /// it, a userspace server sleep in `recv` never sees the request.
    pub fn begin_call_wakes_callee() -> Result<(), String> {
        fresh()?;
        let (client, server) = channels::create().map_err(reason)?;
        // Fork a callee and park it where `recv` parks its task, without
        // switching to it. The caller stays the kernel task, which owns both
        // handles from `create`.
        let callee = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        channels::harness::park(callee, None);
        check!(
            blocked_call(callee, None),
            "the callee was not parked: {:?}",
            task::harness::state(callee)
        );

        let request = parcel(7, flags::SYNC, "ping")?;
        let txn = channels::begin_call(client, 7, &request, None).map_err(reason)?;
        check!(
            task::harness::state(callee) == Some(TaskState::Runnable),
            "begin_call did not wake the parked callee: {:?}",
            task::harness::state(callee)
        );
        check!(
            task::harness::take_wake_reason(callee) == Some(WakeReason::Woken),
            "the callee wake reason is not Woken"
        );

        // The message is exactly where the woken callee's `try_recv` finds it.
        let message = channels::try_recv(server)
            .map_err(reason)?
            .ok_or("the queued request is missing")?;
        check!(
            message.txn == Some(txn) && message.sender == task::current(),
            "the queued request lost its transaction or sender: {message:?}"
        );

        // Finish the transaction so the caller is not left parked, then reap.
        let reply = parcel(8, 0, "pong")?;
        channels::reply(txn, &reply).map_err(reason)?;
        let _ = channels::await_reply(txn).map_err(reason)?;
        task::harness::finish(callee, 0);
        check!(task::reap_child().is_some(), "the callee was not reapable");
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

// ---------------------------------------------------------------------------
// Messenger shared buffers, transfers and fences (issue #67)
// ---------------------------------------------------------------------------

mod ipc_shared_suite {
    use super::*;
    use crate::ipc::channels::{self, Error as ChannelError};
    use crate::ipc::handles::{self, rights, Error as HandleError, HandleKind};
    use crate::ipc::shared::{self, Error as BufferError};
    use crate::task::{TaskState, WakeReason};
    use alloc::vec;
    use libmessenger::{flags, BufferDesc, Encoder, Header, Parcel, VERSION};

    fn buffer_reason(error: BufferError) -> String {
        error.message().into()
    }

    fn channel_reason(error: ChannelError) -> String {
        error.message().into()
    }

    fn handle_reason(error: HandleError) -> String {
        error.message().into()
    }

    /// Every shared-buffer test starts from empty registries and a clean kernel
    /// task. `channels::reset` runs first so it can release the buffer
    /// references held by queued messages before the buffers go away.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        task::harness::reset();
        channels::reset();
        shared::reset();
        handles::reset_for_task(task::current());
        let me = task::current();
        let _ = task::harness::take_wake_reason(me);
        check!(
            task::harness::state(me) == Some(TaskState::Runnable),
            "kernel task is not runnable after reset: {:?}",
            task::harness::state(me)
        );
        Ok(())
    }

    /// Open a channel in the calling task, then mirror the receiving endpoint
    /// handle into `slot`'s table. Handles are per task and there is no
    /// cross-task open call yet, so the harness builds the receiver's half
    /// directly; the transfer under test is the buffer handle, not the
    /// endpoint.
    fn channel_to(slot: usize) -> Result<(u64, u64), String> {
        let (client, server) = channels::create().map_err(channel_reason)?;
        let entry = handles::get(server).map_err(handle_reason)?;
        let caller = task::current();
        task::harness::switch_current(slot);
        let mirror = handles::open(HandleKind::Channel, entry.rights, entry.object_id)
            .map_err(handle_reason)?;
        task::harness::switch_current(caller);
        Ok((client, mirror))
    }

    /// Build a one-way parcel carrying `handles` and `buffers`.
    fn parcel_with_transfers(
        method: u32,
        text: &str,
        handles: Vec<u64>,
        buffers: Vec<BufferDesc>,
    ) -> Result<Vec<u8>, String> {
        let mut body = Encoder::new();
        body.string(1, text).map_err(|error| error.message())?;
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: flags::ONE_WAY,
                interface_id: 0x0bad_cafe,
                method,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body: body.finish(),
            handles,
            buffers,
        };
        let mut bytes = Vec::new();
        parcel.encode(&mut bytes).map_err(|error| error.message())?;
        Ok(bytes)
    }

    /// Spawn a fork child with an empty handle table; the caller reaps it.
    fn spawn_receiver() -> Result<usize, String> {
        let child = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        handles::reset_for_task(child);
        Ok(child)
    }

    /// Finish and reap `child`, returning to the kernel task and resetting the
    /// task table.
    fn reap(child: usize) -> Result<(), String> {
        task::harness::switch_current(task::KERNEL_TASK);
        task::harness::finish(child, 0);
        check!(
            task::reap_child().is_some(),
            "child {child} was not reapable"
        );
        task::harness::reset();
        Ok(())
    }

    /// Create maps the buffer into the creator, the mapping round-trips bytes
    /// and never copies, and close returns the frames to the allocator.
    pub fn buffer_create_write_read() -> Result<(), String> {
        fresh()?;
        let size = 3 * 4096;
        let handle = shared::create(size, shared::flags::READ | shared::flags::WRITE)
            .map_err(buffer_reason)?;
        let info = shared::info(handle).map_err(buffer_reason)?;
        check!(info.size == size, "buffer size is {}", info.size);
        check!(
            info.frames == 3,
            "buffer has {} frames, expected 3",
            info.frames
        );
        check!(info.refs == 1, "creator references are {}", info.refs);
        check!(
            info.mappings == 1,
            "creator mappings are {}, expected 1",
            info.mappings
        );

        let va = shared::map(handle).map_err(buffer_reason)?;
        check!(
            shared::map(handle).map_err(buffer_reason)? == va,
            "map is not idempotent for one task"
        );
        let mut frames = Vec::new();
        for page in 0..3u64 {
            let pte = raw_entry(mem::kernel_table(), va + page * 4096)
                .ok_or_else(|| format!("buffer page {page} is not mapped"))?;
            check!(
                pte & PTE_WRITABLE != 0,
                "buffer page {page} is not writable: {pte:#x}"
            );
            frames.push(pte & PTE_ADDR);
        }
        for offset in 0..size as usize {
            // Safety: the buffer is mapped read/write at `va` for `size` bytes.
            unsafe {
                (va as *mut u8)
                    .add(offset)
                    .write_volatile(pattern_byte(0x5a, offset))
            };
        }
        for offset in (0..size as usize).step_by(37) {
            // Safety: as above.
            let got = unsafe { (va as *const u8).add(offset).read_volatile() };
            check!(
                got == pattern_byte(0x5a, offset),
                "byte {offset} is {got:#x} (mapping corrupted)"
            );
        }

        let stats = shared::stats();
        check!(
            stats.buffers == 1 && stats.bytes == size && stats.mappings == 1,
            "registry stats after create: {stats:?}"
        );
        let process = shared::process_stats(task::current());
        check!(
            process.bytes == size && process.buffers == 1,
            "process stats after create: {process:?}"
        );

        shared::close(handle).map_err(buffer_reason)?;
        check!(
            raw_entry(mem::kernel_table(), va).is_none(),
            "close left the mapping in place"
        );
        for (page, frame) in frames.iter().enumerate() {
            check!(
                mem::frame_refcount(PhysAddr::new(*frame)) == 0,
                "close leaked frame {page} ({frame:#x})"
            );
        }
        check!(
            shared::stats().buffers == 0,
            "close left the buffer in the registry"
        );
        check!(
            shared::info(handle) == Err(BufferError::InvalidHandle),
            "a closed buffer handle still resolves"
        );
        Ok(())
    }

    /// The per-process count and byte quotas are enforced and released as
    /// buffers close.
    pub fn buffer_quota() -> Result<(), String> {
        fresh()?;
        // Count quota: fill with small buffers, then one more is refused.
        let small = 4096u64;
        let mut handles = Vec::new();
        for index in 0..shared::MAX_BUFFERS_PER_PROCESS {
            let handle = shared::create(small, shared::flags::READ | shared::flags::WRITE)
                .map_err(|error| format!("buffer {index}: {}", error.message()))?;
            handles.push(handle);
        }
        check!(
            shared::create(small, shared::flags::READ) == Err(BufferError::Quota),
            "the buffer-count quota was not enforced"
        );
        for handle in handles.drain(..) {
            shared::close(handle).map_err(buffer_reason)?;
        }
        check!(
            shared::process_stats(task::current()).buffers == 0,
            "closing did not release the count quota"
        );

        // Byte quota: one buffer at the limit, then any more is refused.
        let handle = shared::create(
            shared::MAX_BUFFER_BYTES_PER_PROCESS,
            shared::flags::READ | shared::flags::WRITE,
        )
        .map_err(buffer_reason)?;
        check!(
            shared::create(small, shared::flags::READ) == Err(BufferError::Quota),
            "the buffer-byte quota was not enforced"
        );
        shared::close(handle).map_err(buffer_reason)?;
        check!(
            shared::process_stats(task::current()).bytes == 0,
            "closing did not release the byte quota"
        );
        Ok(())
    }

    /// A `SHARE_ONLY` buffer is mapped for its creator but the kernel refuses
    /// to map it in a receiver that got the handle.
    pub fn buffer_share_only_not_mappable() -> Result<(), String> {
        fresh()?;
        let creator = task::current();
        let child = spawn_receiver()?;
        let (client, child_server) = channel_to(child)?;
        let handle = shared::create(4096, shared::flags::READ | shared::flags::SHARE_ONLY)
            .map_err(buffer_reason)?;
        let creator_va = shared::map(handle).map_err(buffer_reason)?;
        check!(
            raw_entry(mem::kernel_table(), creator_va).is_some(),
            "the creator's SHARE_ONLY mapping is missing"
        );

        let bytes = parcel_with_transfers(1, "key material", vec![handle], Vec::new())?;
        channels::send(client, &bytes).map_err(channel_reason)?;
        check!(
            handles::get(handle) == Err(HandleError::InvalidHandle),
            "the transfer did not move the sender's handle"
        );

        task::harness::switch_current(child);
        let message = channels::try_recv(child_server)
            .map_err(channel_reason)?
            .ok_or("the transferred message is missing")?;
        check!(
            message.handles.len() == 1,
            "delivered {} handles, expected 1",
            message.handles.len()
        );
        check!(
            shared::map(message.handles[0]) == Err(BufferError::ShareOnly),
            "a receiver mapped a SHARE_ONLY buffer"
        );

        // Cleanup: the buffer still has the receiver's reference.
        shared::reset();
        handles::reset_for_task(child);
        task::harness::switch_current(creator);
        channels::reset();
        reap(child)?;
        Ok(())
    }

    /// A message transfers handles across two tasks: the sender's numbers are
    /// gone, the receiver's table gets fresh numbers with the same rights, and
    /// a handle without `TRANSFER` is refused.
    pub fn buffer_handle_transfer_rights() -> Result<(), String> {
        fresh()?;
        let creator = task::current();
        let child = spawn_receiver()?;
        let (client, child_server) = channel_to(child)?;

        let movable = handles::open(HandleKind::Object, rights::CALL | rights::TRANSFER, 0xabc)
            .map_err(handle_reason)?;
        let stuck =
            handles::open(HandleKind::Object, rights::CALL, 0xdef).map_err(handle_reason)?;
        let buffer = shared::create(4096, shared::flags::READ | shared::flags::WRITE)
            .map_err(buffer_reason)?;

        // A handle without TRANSFER is refused and nothing moves.
        let refused = parcel_with_transfers(1, "no", vec![stuck], Vec::new())?;
        check!(
            channels::send(client, &refused) == Err(ChannelError::MissingRight),
            "a handle without TRANSFER rights was transferred"
        );
        check!(
            handles::get(stuck).is_ok(),
            "the refused transfer moved the sender's handle"
        );

        let bytes = parcel_with_transfers(1, "yes", vec![movable, buffer], Vec::new())?;
        channels::send(client, &bytes).map_err(channel_reason)?;
        check!(
            handles::get(movable) == Err(HandleError::InvalidHandle)
                && handles::get(buffer) == Err(HandleError::InvalidHandle),
            "the transfer did not move the sender's handles"
        );

        task::harness::switch_current(child);
        let message = channels::try_recv(child_server)
            .map_err(channel_reason)?
            .ok_or("the transferred message is missing")?;
        check!(
            message.handles.len() == 2,
            "delivered {} handles, expected 2",
            message.handles.len()
        );
        let object_handle = message.handles[0];
        let buffer_handle = message.handles[1];
        let object_entry = handles::get(object_handle).map_err(handle_reason)?;
        check!(
            object_entry.kind == HandleKind::Object
                && object_entry.object_id == 0xabc
                && object_entry.rights == rights::CALL | rights::TRANSFER,
            "the received object handle is {object_entry:?}"
        );
        check!(
            handles::duplicate(object_handle, rights::CALL) == Err(HandleError::MissingRight),
            "the received handle did not obey its missing DUPLICATE right"
        );
        let buffer_entry = handles::get(buffer_handle).map_err(handle_reason)?;
        check!(
            buffer_entry.kind == HandleKind::Buffer,
            "the received buffer handle is {buffer_entry:?}"
        );
        // The receiver owns a mapping of the very same frames.
        let receiver_va = shared::map(buffer_handle).map_err(buffer_reason)?;
        check!(
            raw_entry(mem::kernel_table(), receiver_va).is_some(),
            "the receiver's mapping is missing"
        );

        shared::close(buffer_handle).map_err(buffer_reason)?;
        handles::close(object_handle).ok();
        handles::reset_for_task(child);
        task::harness::switch_current(creator);
        handles::close(stuck).ok();
        channels::reset();
        shared::reset();
        reap(child)?;
        Ok(())
    }

    /// A submitted fence resolves a wait, a park is woken by a later submit,
    /// and a wait past its deadline reports `TimedOut`.
    pub fn buffer_fence_submit_wait() -> Result<(), String> {
        fresh()?;
        let handle = shared::create(4096, shared::flags::READ | shared::flags::WRITE)
            .map_err(buffer_reason)?;

        // Nothing submitted yet: an already-expired wait times out.
        check!(
            shared::fence_wait(handle, 1, Some(task::ticks())) == Err(BufferError::TimedOut),
            "fence_wait returned before its sequence was submitted"
        );
        check!(
            task::harness::state(task::current()) == Some(TaskState::Runnable),
            "the waiter stayed parked after the timeout"
        );

        // Park without yielding, then submit: the wake path resolves it.
        let parked = shared::harness::park_wait(handle, 7, None).map_err(buffer_reason)?;
        check!(!parked, "park_wait claimed the sequence was submitted");
        check!(
            matches!(
                task::harness::state(task::current()),
                Some(TaskState::Blocked { .. })
            ),
            "park_wait did not block the waiter"
        );
        shared::fence_submit(handle, 7).map_err(buffer_reason)?;
        check!(
            task::harness::state(task::current()) == Some(TaskState::Runnable),
            "fence_submit did not wake the parked waiter"
        );
        check!(
            task::harness::take_wake_reason(task::current()) == Some(WakeReason::Woken),
            "the fence wake reason is not Woken"
        );
        // The real wait resolves immediately once the sequence is there.
        shared::fence_wait(handle, 7, None).map_err(buffer_reason)?;

        // A deadline sweep wakes a parked waiter with TimedOut.
        let deadline = task::ticks() + 10;
        let parked =
            shared::harness::park_wait(handle, 9, Some(deadline)).map_err(buffer_reason)?;
        check!(!parked, "park_wait claimed the sequence was submitted");
        task::harness::expire_deadlines(deadline);
        check!(
            task::harness::state(task::current()) == Some(TaskState::Runnable),
            "the deadline sweep did not wake the fence waiter"
        );
        check!(
            task::harness::take_wake_reason(task::current()) == Some(WakeReason::TimedOut),
            "the deadline wake reason is not TimedOut"
        );

        // Sequences are monotonic and the meters track the waits.
        check!(
            shared::fence_submit(handle, 3) == Err(BufferError::StaleSequence),
            "a stale fence sequence was accepted"
        );
        let info = shared::info(handle).map_err(buffer_reason)?;
        check!(
            info.submitted == 7 && info.waited == 7,
            "fence state is {info:?}"
        );
        let stats = shared::stats();
        check!(
            stats.fence_waits == 1 && stats.fence_timeouts == 1,
            "fence stats are {stats:?}"
        );
        let process = shared::process_stats(task::current());
        check!(
            process.fence_waits == 1 && process.fence_timeouts == 1,
            "process fence stats are {process:?}"
        );
        shared::close(handle).map_err(buffer_reason)?;
        Ok(())
    }

    /// A buffer handoff moves no data: the receiver's mapping resolves to the
    /// very frames the creator wrote, and the handoff counter advances.
    pub fn buffer_zero_copy_handoff() -> Result<(), String> {
        fresh()?;
        let creator = task::current();
        let child = spawn_receiver()?;
        let (client, child_server) = channel_to(child)?;

        let size = 2 * 4096;
        let handle = shared::create(size, shared::flags::READ | shared::flags::WRITE)
            .map_err(buffer_reason)?;
        let creator_va = shared::map(handle).map_err(buffer_reason)?;
        let mut creator_frames = Vec::new();
        for page in 0..2u64 {
            creator_frames.push(
                frame_of(mem::kernel_table(), creator_va + page * 4096)
                    .map_err(|error| format!("creator page {page}: {error}"))?,
            );
            for offset in 0..4096usize {
                let at = creator_va + page * 4096 + offset as u64;
                // Safety: the buffer is mapped read/write.
                unsafe { (at as *mut u8).write_volatile(pattern_byte(page as u8, offset)) };
            }
        }

        // The transfer moves the creator's handle; its mapping goes with it.
        let bytes = parcel_with_transfers(5, "surface", vec![handle], Vec::new())?;
        channels::send(client, &bytes).map_err(channel_reason)?;

        task::harness::switch_current(child);
        let message = channels::try_recv(child_server)
            .map_err(channel_reason)?
            .ok_or("the transferred message is missing")?;
        check!(
            message.handles.len() == 1,
            "delivered {} handles, expected 1",
            message.handles.len()
        );
        let receiver_va = shared::map(message.handles[0]).map_err(buffer_reason)?;
        check!(
            receiver_va != creator_va,
            "the receiver reused the creator's virtual address"
        );
        for (page, expected) in creator_frames.iter().enumerate() {
            let actual = frame_of(mem::kernel_table(), receiver_va + page as u64 * 4096)
                .map_err(|error| format!("receiver page {page}: {error}"))?;
            check!(
                actual == *expected,
                "page {page} was copied: creator {expected:#x}, receiver {actual:#x}"
            );
            for offset in (0..4096usize).step_by(53) {
                let at = receiver_va + page as u64 * 4096 + offset as u64;
                // Safety: the receiver's mapping is readable.
                let got = unsafe { (at as *const u8).read_volatile() };
                check!(
                    got == pattern_byte(page as u8, offset),
                    "receiver read {got:#x} at page {page} offset {offset}"
                );
            }
        }
        let stats = shared::stats();
        check!(
            stats.handoffs == 1,
            "zero-copy handoffs counted {}, expected 1",
            stats.handoffs
        );
        serial_println!(
            "TEST:ipc_buffer_zero_copy_handoff:INFO:frames={} bytes={size} copies=0",
            creator_frames.len()
        );

        shared::close(message.handles[0]).map_err(buffer_reason)?;
        handles::reset_for_task(child);
        task::harness::switch_current(creator);
        channels::reset();
        shared::reset();
        reap(child)?;
        Ok(())
    }
}
// ---------------------------------------------------------------------------
// Native Messenger syscalls and bootstrap (issue #69)
// ---------------------------------------------------------------------------

mod messenger_suite {
    use super::*;
    use crate::ipc::stats::{FabricStats, FABRIC_STATS_VERSION};
    use crate::ipc::syscalls::{
        self, errno, MsgArgs, MsgResult, MsgStats, OP_CALL, OP_CALL_AWAIT, OP_CALL_BEGIN,
        OP_CANCEL, OP_CLOSE_ENDPOINT, OP_CREATE_PAIR, OP_RECV, OP_REPLY, OP_SEND, OP_STATS,
        OP_TOTALS,
    };
    use crate::ipc::{acl, audit, channels, credentials, handles};
    use crate::task::TaskState;
    use libmessenger::{flags, Decoder, Encoder, Header, Kind, Parcel, VERSION};

    const IFACE: u64 = 0x6969_6969_6969_6969;

    /// Scratch user address space for the syscall tests. `dispatch` validates
    /// pointers against the active CR3, so each test installs a fresh table and
    /// restores the kernel's afterwards.
    const SPACE: u64 = 0x0040_0000;
    const SPACE_PAGES: u64 = 8;
    /// Blocks inside the scratch space, one per page so page-crossing copies
    /// are not a factor in these tests.
    const ARGS: u64 = SPACE;
    const RESULT: u64 = SPACE + 0x100;
    const REQUEST: u64 = SPACE + 0x1000;
    const RECV_BUF: u64 = SPACE + 0x2000;
    const REPLY_BUF: u64 = SPACE + 0x3000;
    const STATS_BUF: u64 = SPACE + 0x4000;

    /// Each test starts from the bring-up state: kernel task current and
    /// runnable, no handles, channels, policy, audit events, or bootstrap.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        for slot in 0..task::MAX_TASKS {
            handles::reset_for_task(slot);
        }
        channels::reset();
        syscalls::bootstrap::reset();
        credentials::reset_for_task(task::KERNEL_TASK);
        acl::load(&[]);
        audit::reset();
        audit::set_trace(false);
        // A failed earlier test can leave the kernel task parked; a stale wake
        // reason must not leak into this one.
        task::wake_task(task::KERNEL_TASK);
        let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
        Ok(())
    }

    /// Friendly-message adapter for `Result` plumbing.
    fn reason(error: channels::Error) -> String {
        error.message().into()
    }

    /// Two's-complement `-errno` as the syscall returns it in `rax`.
    fn failed(code: i64) -> u64 {
        (code as u64).wrapping_neg()
    }

    /// Run `f` with [`SPACE`] mapped into a fresh address space installed as
    /// CR3, exactly as a real syscall from a user task would find it.
    fn in_space<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
        let kernel = mem::kernel_table();
        let table = mem::new_user_table().ok_or("new_user_table failed")?;
        process::map_range(table, SPACE, SPACE + SPACE_PAGES * 4096).map_err(to_string)?;
        mem::switch_to(table);
        let outcome = f();
        mem::switch_to(kernel);
        mem::free_user_table(table);
        outcome
    }

    /// Encode a parcel whose body carries one string field.
    fn parcel(method: u32, parcel_flags: u16, text: &str) -> Result<Vec<u8>, String> {
        let mut body = Encoder::new();
        body.string(1, text).map_err(|error| error.message())?;
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: parcel_flags,
                interface_id: IFACE,
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
    fn string_field(bytes: &[u8]) -> Result<String, String> {
        let parcel = Parcel::decode(bytes).map_err(|error| error.message())?;
        let mut decoder = Decoder::new(&parcel.body);
        while let Some(field) = decoder.next().map_err(|error| error.message())? {
            if field.kind == Kind::String {
                return Ok(field.as_str().map_err(|error| error.message())?.into());
            }
        }
        Err("parcel body has no string field".into())
    }

    /// Write bytes into the installed scratch space.
    fn write_bytes(va: u64, bytes: &[u8]) {
        // Safety: the scratch pages are mapped writable while installed.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), va as *mut u8, bytes.len()) };
    }

    /// Read bytes from the installed scratch space.
    fn read_bytes(va: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        out.resize(len, 0);
        // Safety: the scratch pages are mapped readable while installed.
        unsafe { core::ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), len) };
        out
    }

    /// Run one op through the native gate with the args block at [`ARGS`] and
    /// decode the result block.
    fn syscall(op: u64, args: &MsgArgs) -> (u64, MsgResult) {
        write_bytes(ARGS, &args.to_bytes());
        let code = process::dispatch_for_test(5, op, ARGS, RESULT);
        let result = MsgResult::from_bytes(&read_bytes(RESULT, 64))
            .expect("the kernel wrote a malformed result block");
        (code, result)
    }

    /// The full syscall path: a synchronous echo call and reply, byte for byte,
    /// plus one-way send, cancel, stats, and close. Prints `MSG:ECHO:PASS` so
    /// CI can grep the round trip.
    pub fn syscall_echo() -> Result<(), String> {
        fresh()?;
        in_space(|| -> Result<(), String> {
            // 1. Create the pair through the syscall; both handles land in the
            //    calling (kernel) task's table.
            let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
            check!(code == 0 && created.status == 0, "create_pair -> {code:#x}");
            let (client, server) = (created.value, created.aux);
            check!(client != server, "create_pair reused handle {client}");

            // 2. Begin a synchronous call with a "ping" request parcel.
            let request = parcel(7, flags::SYNC, "ping")?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                handle: client,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, begun) = syscall(OP_CALL_BEGIN, &args);
            check!(code == 0, "call_begin -> {code:#x}");
            let txn = begun.value;
            check!(txn != 0, "call_begin returned transaction 0");

            // 3. The server receives exactly the request bytes.
            let args = MsgArgs {
                handle: server,
                buf_ptr: RECV_BUF,
                buf_cap: 4096,
                ..MsgArgs::default()
            };
            let (code, received) = syscall(OP_RECV, &args);
            check!(code == 0, "recv -> {code:#x}");
            check!(
                received.value == txn,
                "recv transaction is {} (expected {txn})",
                received.value
            );
            check!(
                received.aux == task::current() as u64,
                "recv sender is {} (expected {})",
                received.aux,
                task::current()
            );
            let got = read_bytes(RECV_BUF, received.bytes as usize);
            check!(got == request, "request bytes changed in flight");
            check!(string_field(&got)? == "ping", "request payload changed");

            // 4. Reply with "pong"; the caller awaits the exact reply parcel.
            let reply = parcel(8, 0, "pong")?;
            write_bytes(REPLY_BUF, &reply);
            let args = MsgArgs {
                txn_id: txn,
                parcel_ptr: REPLY_BUF,
                parcel_len: reply.len() as u64,
                ..MsgArgs::default()
            };
            let (code, _) = syscall(OP_REPLY, &args);
            check!(code == 0, "reply -> {code:#x}");

            let args = MsgArgs {
                txn_id: txn,
                buf_ptr: RECV_BUF,
                buf_cap: 4096,
                ..MsgArgs::default()
            };
            let (code, awaited) = syscall(OP_CALL_AWAIT, &args);
            check!(code == 0, "call_await -> {code:#x}");
            let got = read_bytes(RECV_BUF, awaited.bytes as usize);
            check!(got == reply, "reply bytes changed on the way back");
            check!(string_field(&got)? == "pong", "reply payload changed");

            // 5. One-way send and receive: same bytes, no transaction id.
            let note = parcel(11, flags::ONE_WAY, "note")?;
            write_bytes(REQUEST, &note);
            let args = MsgArgs {
                handle: client,
                parcel_ptr: REQUEST,
                parcel_len: note.len() as u64,
                ..MsgArgs::default()
            };
            let (code, _) = syscall(OP_SEND, &args);
            check!(code == 0, "send -> {code:#x}");
            let args = MsgArgs {
                handle: server,
                buf_ptr: RECV_BUF,
                buf_cap: 4096,
                ..MsgArgs::default()
            };
            let (code, received) = syscall(OP_RECV, &args);
            check!(
                code == 0 && received.value == 0,
                "one-way recv -> {code:#x}"
            );
            check!(
                read_bytes(RECV_BUF, received.bytes as usize) == note,
                "one-way bytes changed in flight"
            );

            // 6. Counters agree with one call and one reply.
            let args = MsgArgs {
                buf_ptr: STATS_BUF,
                buf_cap: MsgStats::SIZE as u64,
                ..MsgArgs::default()
            };
            let (code, result) = syscall(OP_STATS, &args);
            check!(
                code == 0 && result.bytes as usize == MsgStats::SIZE,
                "stats -> {code:#x}"
            );
            let stats = MsgStats::from_bytes(&read_bytes(STATS_BUF, MsgStats::SIZE))
                .ok_or("bad stats block")?;
            check!(
                stats.calls == 1 && stats.replies == 1 && stats.outstanding == 0,
                "counters after one echo: {stats:?}"
            );
            check!(stats.queued == 0, "channel not drained: {stats:?}");

            serial_println!("MSG:ECHO:PASS");

            // 7. Cancel a registered call; the await reports the cancellation.
            let stuck = parcel(12, flags::SYNC, "stuck")?;
            write_bytes(REQUEST, &stuck);
            let args = MsgArgs {
                handle: client,
                parcel_ptr: REQUEST,
                parcel_len: stuck.len() as u64,
                ..MsgArgs::default()
            };
            let (code, begun) = syscall(OP_CALL_BEGIN, &args);
            check!(code == 0, "begin(stuck) -> {code:#x}");
            let args = MsgArgs {
                txn_id: begun.value,
                ..MsgArgs::default()
            };
            check!(syscall(OP_CANCEL, &args).0 == 0, "cancel failed");
            let args = MsgArgs {
                txn_id: begun.value,
                buf_ptr: RECV_BUF,
                buf_cap: 4096,
                ..MsgArgs::default()
            };
            check!(
                syscall(OP_CALL_AWAIT, &args).0 == failed(errno::ECANCELED),
                "await after cancel was not -ECANCELED"
            );

            // 8. Closing both ends frees the handles.
            let args = MsgArgs {
                handle: client,
                ..MsgArgs::default()
            };
            check!(
                syscall(OP_CLOSE_ENDPOINT, &args).0 == 0,
                "close(client) failed"
            );
            let args = MsgArgs {
                handle: server,
                ..MsgArgs::default()
            };
            check!(
                syscall(OP_CLOSE_ENDPOINT, &args).0 == 0,
                "close(server) failed"
            );
            Ok(())
        })
    }

    /// The blocking `call` op parks through the timer gate and maps the
    /// channel's timeout to `-ETIMEDOUT`.
    pub fn syscall_timeout() -> Result<(), String> {
        fresh()?;
        in_space(|| -> Result<(), String> {
            let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
            check!(code == 0, "create_pair -> {code:#x}");
            let request = parcel(7, flags::SYNC, "nobody home")?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                handle: created.value,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                buf_ptr: RECV_BUF,
                buf_cap: 4096,
                deadline: task::ticks() + 1,
                ..MsgArgs::default()
            };
            let (code, result) = syscall(OP_CALL, &args);
            check!(
                code == failed(errno::ETIMEDOUT),
                "call -> {code:#x}, expected -ETIMEDOUT"
            );
            check!(
                result.status == -errno::ETIMEDOUT,
                "timeout status is {}",
                result.status
            );
            check!(
                channels::stats().timeouts == 1,
                "the timeout was not counted: {:?}",
                channels::stats()
            );
            Ok(())
        })
    }

    /// A parcel-bearing op goes through the ACL hook: with a non-empty policy
    /// that does not cover the caller, the call is `-EACCES`, the channel is
    /// untouched, and the denial is audited.
    pub fn syscall_denied() -> Result<(), String> {
        fresh()?;
        acl::load(&[acl::Rule {
            actor: 2000,
            interface_id: IFACE,
            method: 7,
            allow: true,
        }]);
        credentials::set(
            task::KERNEL_TASK,
            credentials::Cred::new(1000, 100, 0, 0, 0),
        );
        in_space(|| -> Result<(), String> {
            let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
            check!(code == 0, "create_pair -> {code:#x}");
            let request = parcel(7, flags::SYNC, "blocked")?;
            write_bytes(REQUEST, &request);
            let before = audit::count();
            let args = MsgArgs {
                handle: created.value,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, result) = syscall(OP_CALL_BEGIN, &args);
            check!(
                code == failed(errno::EACCES),
                "denied call_begin -> {code:#x}"
            );
            check!(
                result.status == -errno::EACCES,
                "denied status is {}",
                result.status
            );
            let stats = channels::stats();
            check!(
                stats.calls == 0 && stats.queued == 0,
                "a denied call touched the channel: {stats:?}"
            );
            check!(
                audit::count() == before + 1,
                "the denial was not audited: {} -> {}",
                before,
                audit::count()
            );
            let event = *audit::recent(1).first().ok_or("no audit event")?;
            check!(
                !event.allow && event.interface_id == IFACE && event.method == 7,
                "the denial event is {event:?}"
            );
            check!(
                event.reason_code == acl::reason::DEFAULT_DENY,
                "the denial reason code is {}",
                event.reason_code
            );
            Ok(())
        })
    }

    /// Every malformed pointer or length returns an error and leaves the task
    /// runnable: no kernel fault, no panic, no side effect.
    pub fn syscall_bad_pointer() -> Result<(), String> {
        fresh()?;
        in_space(|| -> Result<(), String> {
            let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
            check!(code == 0, "create_pair -> {code:#x}");
            let request = parcel(7, flags::SYNC, "x")?;
            write_bytes(REQUEST, &request);

            // 1. An unmapped parcel pointer fails cleanly and enqueues nothing.
            let args = MsgArgs {
                handle: created.value,
                parcel_ptr: 0xdead_0000,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, result) = syscall(OP_CALL_BEGIN, &args);
            check!(
                code == failed(errno::EFAULT),
                "unmapped parcel -> {code:#x}"
            );
            check!(
                result.status == -errno::EFAULT,
                "bad-pointer status is {}",
                result.status
            );
            check!(
                channels::stats().queued == 0,
                "a bad pointer enqueued a message"
            );

            // 2. An unmapped args block: the result block cannot be trusted
            //    either, so the return register is the only report.
            let code = process::dispatch_for_test(5, OP_CREATE_PAIR, 0xdead_0000, RESULT);
            check!(code == failed(errno::EFAULT), "unmapped args -> {code:#x}");

            // 3. An unmapped result block is refused before the op runs.
            let code = process::dispatch_for_test(5, OP_CREATE_PAIR, ARGS, 0xdead_0000);
            check!(
                code == failed(errno::EFAULT),
                "unmapped result -> {code:#x}"
            );

            // 4. An oversized parcel length is refused before any copy.
            let args = MsgArgs {
                handle: created.value,
                parcel_ptr: REQUEST,
                parcel_len: (libmessenger::MAX_PARCEL_BYTES + 1) as u64,
                ..MsgArgs::default()
            };
            let (code, _) = syscall(OP_CALL_BEGIN, &args);
            check!(
                code == failed(errno::E2BIG),
                "oversized parcel -> {code:#x}"
            );

            // 5. A garbage parcel is caught by the codec, not the channel.
            write_bytes(REQUEST, &[0u8; 48]);
            let args = MsgArgs {
                handle: created.value,
                parcel_ptr: REQUEST,
                parcel_len: 48,
                ..MsgArgs::default()
            };
            let (code, _) = syscall(OP_CALL_BEGIN, &args);
            check!(code == failed(errno::EINVAL), "garbage parcel -> {code:#x}");

            check!(
                task::harness::state(task::KERNEL_TASK) == Some(TaskState::Runnable),
                "the task did not survive the bad pointers"
            );
            check!(
                channels::stats().calls == 0,
                "a refused call registered a transaction"
            );
            Ok(())
        })
    }

    /// The bootstrap flow: one kernel-created pair, the client end claimed by
    /// a userspace task exactly once, the service end served by the kernel
    /// stub, and a reply that round-trips.
    pub fn bootstrap_claim() -> Result<(), String> {
        fresh()?;
        syscalls::bootstrap::create().map_err(to_string)?;
        check!(
            syscalls::bootstrap::service_handle().is_some(),
            "create left no service handle"
        );

        let child = task::spawn_fork().map_err(to_string)?;
        task::harness::switch_current(child);
        let client =
            syscalls::bootstrap::claim_client().map_err(|code| format!("claim failed: {code}"))?;
        check!(
            handles::count_for_task(child) == 1,
            "child holds {} handles, expected 1",
            handles::count_for_task(child)
        );
        check!(
            syscalls::bootstrap::claim_client() == Err(errno::EBUSY),
            "the client end was claimed twice"
        );

        // The service end stays kernel-side; the stub echoes the client.
        let request = parcel(1, flags::SYNC, "bootstrap")?;
        channels::send(client, &request).map_err(reason)?;
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            syscalls::bootstrap::claim_client() == Err(errno::EPERM),
            "the kernel task claimed the client end"
        );
        check!(
            syscalls::bootstrap::stub_serve().map_err(reason)?,
            "the stub found no request"
        );
        check!(
            !syscalls::bootstrap::stub_serve().map_err(reason)?,
            "the stub served the same request twice"
        );

        task::harness::switch_current(child);
        let reply = channels::recv(client, None).map_err(reason)?;
        check!(
            reply.bytes == request,
            "the stub reply differs from the request"
        );

        task::harness::switch_current(task::KERNEL_TASK);
        task::harness::finish(child, 0);
        check!(
            task::reap_child().is_some(),
            "the bootstrap child was not reapable"
        );
        Ok(())
    }

    /// `OP_STATS` serves the versioned `FabricStats` snapshot when the caller
    /// offers a snapshot-sized buffer, keeps the 64-byte v1 counters for small
    /// buffers and for a per-channel handle, and `OP_TOTALS` always returns
    /// the compact global counters.
    pub fn syscall_fabric_stats() -> Result<(), String> {
        fresh()?;
        in_space(|| -> Result<(), String> {
            let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
            check!(code == 0, "create_pair -> {code:#x}");

            // Version 2: a snapshot-sized buffer selects the rich block.
            write_bytes(STATS_BUF, &vec![0u8; FabricStats::SIZE]);
            let args = MsgArgs {
                buf_ptr: STATS_BUF,
                buf_cap: FabricStats::SIZE as u64,
                ..MsgArgs::default()
            };
            let (code, result) = syscall(OP_STATS, &args);
            check!(
                code == 0 && result.bytes as usize == FabricStats::SIZE,
                "fabric stats -> {code:#x}, {} bytes",
                result.bytes
            );
            let snap = FabricStats::from_bytes(&read_bytes(STATS_BUF, FabricStats::SIZE))
                .ok_or("bad fabric stats block")?;
            check!(
                snap.version == FABRIC_STATS_VERSION,
                "snapshot version is {}",
                snap.version
            );
            check!(
                snap.channels == 1 && snap.endpoints == 2,
                "snapshot counts are {snap:?}"
            );
            check!(
                snap.handles == 2 && snap.handles_per_task[task::current()] == 2,
                "snapshot lost the pair handles: {snap:?}"
            );
            check!(
                snap.audit_last_hash == audit::last_hash(),
                "snapshot hash is not the live chain head"
            );

            // Version 1, per channel: a 64-byte buffer and a handle keep the
            // compact counters.
            let args = MsgArgs {
                handle: created.value,
                buf_ptr: STATS_BUF,
                buf_cap: MsgStats::SIZE as u64,
                ..MsgArgs::default()
            };
            let (code, result) = syscall(OP_STATS, &args);
            check!(
                code == 0 && result.bytes as usize == MsgStats::SIZE,
                "channel stats -> {code:#x}, {} bytes",
                result.bytes
            );

            // The dedicated totals op serves the same compact shape globally.
            let args = MsgArgs {
                buf_ptr: STATS_BUF,
                buf_cap: MsgStats::SIZE as u64,
                ..MsgArgs::default()
            };
            let (code, result) = syscall(OP_TOTALS, &args);
            check!(
                code == 0 && result.bytes as usize == MsgStats::SIZE,
                "totals -> {code:#x}, {} bytes",
                result.bytes
            );
            let totals = MsgStats::from_bytes(&read_bytes(STATS_BUF, MsgStats::SIZE))
                .ok_or("bad totals block")?;
            check!(
                totals.calls == 0 && totals.replies == 0 && totals.drops == 0,
                "totals after pair creation: {totals:?}"
            );
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// Fabric observability snapshot (issue #70)
// ---------------------------------------------------------------------------

mod stats_suite {
    use super::*;
    use crate::ipc::stats::{self, FABRIC_STATS_VERSION};
    use crate::ipc::{acl, audit, channels, handles, shared};
    use libmessenger::{flags, Encoder, Header, Parcel, VERSION};

    const IFACE: u64 = 0x0abc_0def_1234_5678;

    /// Friendly-message adapter for channel errors.
    fn reason(error: channels::Error) -> String {
        error.message().into()
    }

    /// Friendly-message adapter for shared-buffer errors.
    fn buffer_reason(error: shared::Error) -> String {
        error.message().into()
    }

    /// Every stats test starts from an empty fabric with the kernel task
    /// current and runnable.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        stats::reset();
        task::wake_task(task::KERNEL_TASK);
        let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
        Ok(())
    }

    /// Encode a parcel whose body carries one string field.
    fn parcel(method: u32, parcel_flags: u16, text: &str) -> Result<Vec<u8>, String> {
        let mut body = Encoder::new();
        body.string(1, text).map_err(|error| error.message())?;
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: parcel_flags,
                interface_id: IFACE,
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

    /// A snapshot reports exactly the handles, channels, buffer, policy rules
    /// and audited denial the test created, with the live chain head.
    pub fn snapshot_reflects_objects() -> Result<(), String> {
        fresh()?;
        let (client, server) = channels::create().map_err(reason)?;
        let buffer = shared::create(4096, shared::flags::READ | shared::flags::WRITE)
            .map_err(buffer_reason)?;
        acl::load(&[
            acl::Rule {
                actor: 0,
                interface_id: IFACE,
                method: 3,
                allow: false,
            },
            acl::Rule {
                actor: acl::ANY_ACTOR,
                interface_id: acl::ANY_INTERFACE,
                method: acl::ANY_METHOD,
                allow: true,
            },
        ]);
        let before = stats::snapshot();
        check!(
            before.audit_denies == 0,
            "denials start at {}",
            before.audit_denies
        );
        let decision = crate::ipc::authorize(task::KERNEL_TASK, IFACE, 3, 42);
        check!(decision.denied(), "the explicit deny rule was not applied");

        let snap = stats::snapshot();
        check!(
            snap.version == FABRIC_STATS_VERSION,
            "snapshot version is {}",
            snap.version
        );
        check!(
            snap.services == 0,
            "no bootstrap was created but services is {}",
            snap.services
        );
        check!(
            snap.channels == 1 && snap.endpoints == 2,
            "channel counts are channels {} endpoints {}",
            snap.channels,
            snap.endpoints
        );
        check!(
            snap.handles == 3 && snap.handles_per_task[task::KERNEL_TASK] == 3,
            "handle counts are total {} slot {}",
            snap.handles,
            snap.handles_per_task[task::KERNEL_TASK]
        );
        check!(
            snap.tasks[task::KERNEL_TASK].live == 1,
            "the kernel slot is not marked live"
        );
        check!(
            snap.tasks[task::KERNEL_TASK].buffers == 1
                && snap.tasks[task::KERNEL_TASK].buffer_bytes == 4096,
            "slot usage is {:?}",
            snap.tasks[task::KERNEL_TASK]
        );
        check!(
            snap.buffers == 1 && snap.buffer_bytes == 4096 && snap.buffer_mappings == 1,
            "buffer counts are {:?}",
            (snap.buffers, snap.buffer_bytes, snap.buffer_mappings)
        );
        check!(
            snap.acl_rules == 2 && snap.acl_loaded == 1,
            "ACL state is rules {} loaded {}",
            snap.acl_rules,
            snap.acl_loaded
        );
        check!(
            snap.audit_count == 1
                && snap.audit_total == 1
                && snap.audit_denies == 1
                && snap.audit_allows == 0,
            "audit state is {:?}",
            (snap.audit_count, snap.audit_total, snap.audit_denies)
        );
        check!(
            snap.audit_last_hash == audit::last_hash()
                && snap.audit_last_hash != audit::GENESIS_HASH,
            "the snapshot hash is not the live chain head"
        );

        handles::close(client).ok();
        handles::close(server).ok();
        shared::close(buffer).ok();
        Ok(())
    }

    /// `stats::reset` restores every counter — and the audit chain — to the
    /// bring-up state.
    pub fn reset_restores_zeros() -> Result<(), String> {
        fresh()?;
        channels::create().map_err(reason)?;
        shared::create(4096, shared::flags::READ).map_err(buffer_reason)?;
        acl::load(&[acl::Rule {
            actor: acl::ANY_ACTOR,
            interface_id: acl::ANY_INTERFACE,
            method: acl::ANY_METHOD,
            allow: true,
        }]);
        audit::record(audit::AuditEvent {
            ticks: 1,
            actor_slot: task::KERNEL_TASK,
            uid: 0,
            label_id: 0,
            interface_id: IFACE,
            method: 1,
            allow: false,
            reason_code: acl::reason::DEFAULT_DENY,
            txn_id: 7,
        });
        let before = stats::snapshot();
        check!(
            before.channels == 1 && before.buffers == 1 && before.audit_total == 1,
            "the fabric was not populated before reset: {before:?}"
        );

        stats::reset();
        let snap = stats::snapshot();
        check!(
            snap.version == FABRIC_STATS_VERSION,
            "snapshot version is {}",
            snap.version
        );
        check!(
            snap.services == 0 && snap.channels == 0 && snap.endpoints == 0,
            "reset left services/channels/endpoints: {snap:?}"
        );
        check!(
            snap.handles == 0 && snap.handles_per_task.iter().all(|held| *held == 0),
            "reset left handles: {snap:?}"
        );
        check!(
            snap.buffers == 0 && snap.buffer_bytes == 0 && snap.buffer_mappings == 0,
            "reset left buffers: {snap:?}"
        );
        check!(
            snap.acl_rules == 0 && snap.acl_loaded == 0,
            "reset left the ACL loaded: {snap:?}"
        );
        check!(
            snap.audit_count == 0
                && snap.audit_total == 0
                && snap.audit_denies == 0
                && snap.audit_allows == 0,
            "reset left audit counters: {snap:?}"
        );
        check!(
            snap.audit_last_hash == audit::GENESIS_HASH,
            "reset left the chain head at {:#x}",
            snap.audit_last_hash
        );
        Ok(())
    }

    /// An accepted one-way message and a synchronous call/reply move the
    /// message counters; a policy denial moves the audit deny counter without
    /// touching the channel counters.
    pub fn counters_follow_calls_and_denials() -> Result<(), String> {
        fresh()?;
        let (client, server) = channels::create().map_err(reason)?;
        let before = stats::snapshot();

        let note = parcel(7, flags::ONE_WAY, "note")?;
        channels::send(client, &note).map_err(reason)?;
        let message = channels::try_recv(server)
            .map_err(reason)?
            .ok_or("the one-way message was not delivered")?;
        check!(
            message.sender == task::KERNEL_TASK,
            "sender is {}, expected {}",
            message.sender,
            task::KERNEL_TASK
        );
        let after_send = stats::snapshot();
        check!(
            after_send.one_way == before.one_way + 1,
            "one-way counter is {}, expected {}",
            after_send.one_way,
            before.one_way + 1
        );
        check!(
            after_send.calls == before.calls,
            "the one-way send counted as a call"
        );

        let request = parcel(9, flags::SYNC, "ping")?;
        let txn = channels::begin_call(client, 9, &request, None).map_err(reason)?;
        let _ = channels::recv(server, None).map_err(reason)?;
        channels::reply(txn, &request).map_err(reason)?;
        let _ = task::harness::take_wake_reason(task::current());
        let _ = channels::await_reply(txn).map_err(reason)?;
        let after_call = stats::snapshot();
        check!(
            after_call.calls == after_send.calls + 1
                && after_call.replies == after_send.replies + 1,
            "call counters are calls {} replies {}",
            after_call.calls,
            after_call.replies
        );
        check!(
            after_call.outstanding == 0,
            "outstanding is {} after the reply",
            after_call.outstanding
        );

        acl::load(&[acl::Rule {
            actor: 9999,
            interface_id: IFACE,
            method: 9,
            allow: true,
        }]);
        let denied = crate::ipc::authorize(task::KERNEL_TASK, IFACE, 9, 77);
        check!(denied.denied(), "the unmatched call was not denied");
        let after_denial = stats::snapshot();
        check!(
            after_denial.audit_denies == after_call.audit_denies + 1,
            "deny counter is {}, expected {}",
            after_denial.audit_denies,
            after_call.audit_denies + 1
        );
        check!(
            after_denial.calls == after_call.calls && after_denial.drops == after_call.drops,
            "a denied call touched the channel counters"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Name registry (issue #89)
// ---------------------------------------------------------------------------

mod registry_suite {
    use super::*;
    use crate::ipc::registry::{self, Error as RegistryError};
    use crate::ipc::syscalls::{
        errno, MsgArgs, MsgResult, OP_LIST, OP_REGISTER, OP_RESOLVE, OP_UNREGISTER,
    };
    use crate::ipc::{acl, audit, channels, credentials, handles};
    use crate::task::TaskState;
    use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

    /// Friendly-message adapter for each error type the suite plumb through
    /// `Result<_, String>`; a trait keeps `map_err` call sites terse and typed.
    trait Friendly {
        fn friendly(self) -> String;
    }

    impl Friendly for libmessenger::Error {
        fn friendly(self) -> String {
            self.message().into()
        }
    }

    impl Friendly for channels::Error {
        fn friendly(self) -> String {
            self.message().into()
        }
    }

    impl Friendly for handles::Error {
        fn friendly(self) -> String {
            self.message().into()
        }
    }

    impl Friendly for RegistryError {
        fn friendly(self) -> String {
            self.message().into()
        }
    }

    fn friendly<E: Friendly>(error: E) -> String {
        error.friendly()
    }

    /// Scratch user address space for the syscall-level test, private per test
    /// because `in_space` installs and frees a fresh table around it.
    const SPACE: u64 = 0x0050_0000;
    const SPACE_PAGES: u64 = 8;
    const ARGS: u64 = SPACE;
    const RESULT: u64 = SPACE + 0x100;
    const REQUEST: u64 = SPACE + 0x1000;
    const LIST_BUF: u64 = SPACE + 0x2000;

    /// Every registry test starts from an empty fabric with the kernel task
    /// current and runnable, so counts are deterministic.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        for slot in 0..task::MAX_TASKS {
            handles::reset_for_task(slot);
        }
        channels::reset();
        registry::reset();
        credentials::reset_for_task(task::KERNEL_TASK);
        acl::load(&[]);
        audit::reset();
        audit::set_trace(false);
        task::wake_task(task::KERNEL_TASK);
        let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
        Ok(())
    }

    /// Friendly-message adapter for registry plumbing.
    fn reason(error: RegistryError) -> String {
        error.message().into()
    }

    /// Encode a registry request parcel whose body is already built.
    fn encode_parcel(method: u32, body: Encoder) -> Result<Vec<u8>, String> {
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: 0,
                interface_id: registry::INTERFACE,
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
        parcel.encode(&mut bytes).map_err(friendly)?;
        Ok(bytes)
    }

    /// A request body carrying one name field.
    fn string_parcel(method: u32, text: &str) -> Result<Vec<u8>, String> {
        let mut body = Encoder::new();
        body.string(registry::field::NAME, text).map_err(friendly)?;
        encode_parcel(method, body)
    }

    /// A register request: name, endpoint handle, interface array and lease.
    fn register_parcel(
        name: &str,
        endpoint: u64,
        interfaces: &[u64],
        lease: u64,
    ) -> Result<Vec<u8>, String> {
        let mut body = Encoder::new();
        body.string(registry::field::NAME, name).map_err(friendly)?;
        body.u64(registry::field::ENDPOINT, endpoint)
            .map_err(friendly)?;
        let mut array = Encoder::new();
        for interface in interfaces {
            array
                .u64(registry::field::INTERFACES, *interface)
                .map_err(friendly)?;
        }
        body.array(registry::field::INTERFACES, &array)
            .map_err(friendly)?;
        body.u64(registry::field::LEASE_TICKS, lease)
            .map_err(friendly)?;
        encode_parcel(registry::method::REGISTER, body)
    }

    /// Run `f` with [`SPACE`] mapped into a fresh address space installed as
    /// CR3, exactly as a real syscall from a user task would find it.
    fn in_space<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
        let kernel = mem::kernel_table();
        let table = mem::new_user_table().ok_or("new_user_table failed")?;
        process::map_range(table, SPACE, SPACE + SPACE_PAGES * 4096).map_err(to_string)?;
        mem::switch_to(table);
        let outcome = f();
        mem::switch_to(kernel);
        mem::free_user_table(table);
        outcome
    }

    /// Two's-complement `-errno` as the syscall returns it in `rax`.
    fn failed(code: i64) -> u64 {
        (code as u64).wrapping_neg()
    }

    /// Write bytes into the installed scratch space.
    fn write_bytes(va: u64, bytes: &[u8]) {
        // Safety: the scratch pages are mapped writable while installed.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), va as *mut u8, bytes.len()) };
    }

    /// Read bytes from the installed scratch space.
    fn read_bytes(va: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        out.resize(len, 0);
        // Safety: the scratch pages are mapped readable while installed.
        unsafe { core::ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), len) };
        out
    }

    /// One op through the native gate with the args block at [`ARGS`].
    fn dispatch(op: u64, args: &MsgArgs) -> (u64, MsgResult) {
        write_bytes(ARGS, &args.to_bytes());
        let code = process::dispatch_for_test(5, op, ARGS, RESULT);
        let result = MsgResult::from_bytes(&read_bytes(RESULT, 64))
            .expect("the kernel wrote a malformed result block");
        (code, result)
    }

    /// Register/resolve round-trips a name and, more importantly, duplicates
    /// the endpoint into the *resolver's* table: the child gets its own handle
    /// to the object the owner published, and an echo call through that handle
    /// reaches the owner's side. Owner death then releases the name.
    pub fn register_resolve_roundtrip() -> Result<(), String> {
        fresh()?;

        // The child owns the service: it creates the channel, keeps the
        // receiving side and publishes the callable side under the name.
        let child = task::spawn_fork().map_err(to_string)?;
        task::harness::switch_current(child);
        let (service, callable) = channels::create().map_err(friendly)?;
        let published = handles::get(callable).map_err(friendly)?;
        registry::register(
            child,
            "os.example.echo",
            published.kind,
            published.rights,
            published.object_id,
            &[0xfeed],
            0,
        )
        .map_err(reason)?;

        // The kernel resolves the name; the handle is open in *its* table, not
        // the owner's, and names the same object.
        task::harness::switch_current(task::KERNEL_TASK);
        let resolved = registry::resolve(task::KERNEL_TASK, "os.example.echo").map_err(reason)?;
        check!(
            resolved != callable,
            "resolve handed back the owner's own handle {callable}"
        );
        let copy = handles::get(resolved).map_err(friendly)?;
        check!(
            copy.object_id == published.object_id && copy.kind == published.kind,
            "the resolved handle names a different object"
        );
        check!(
            handles::count_for_task(task::KERNEL_TASK) == 1,
            "the resolver holds {} handles, expected 1",
            handles::count_for_task(task::KERNEL_TASK)
        );

        // Echo through the resolved name: the kernel calls, the owner answers.
        let request = string_parcel(7, "ping")?;
        let txn = channels::begin_call(resolved, 7, &request, None).map_err(friendly)?;
        check!(
            matches!(
                task::harness::state(task::KERNEL_TASK),
                Some(TaskState::Blocked { .. })
            ),
            "begin_call did not park the caller"
        );
        task::harness::switch_current(child);
        let message = channels::recv(service, None).map_err(friendly)?;
        check!(
            message.txn == Some(txn),
            "the request arrived with transaction {:?}",
            message.txn
        );
        check!(
            message.sender == task::KERNEL_TASK,
            "the sender is {}, expected {}",
            message.sender,
            task::KERNEL_TASK
        );
        channels::reply(txn, &request).map_err(friendly)?;
        task::harness::switch_current(task::KERNEL_TASK);
        let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
        let reply = channels::await_reply(txn).map_err(friendly)?;
        check!(reply == request, "the echo reply changed in flight");

        // Owner death: the slot is gone after the reap, so the name is too.
        task::harness::finish(child, 0);
        check!(task::reap_child().is_some(), "the owner was not reapable");
        check!(
            registry::resolve(task::KERNEL_TASK, "os.example.echo")
                == Err(RegistryError::UnknownName),
            "the name outlived its owner"
        );
        check!(registry::list().is_empty(), "list kept a dead owner's name");
        Ok(())
    }

    /// An unknown name reports the dedicated, friendly error rather than a
    /// generic lookup failure.
    pub fn unknown_name_friendly() -> Result<(), String> {
        fresh()?;
        let error = registry::resolve(task::KERNEL_TASK, "no.such.service").unwrap_err();
        check!(
            error == RegistryError::UnknownName,
            "resolve of an unknown name returned {error:?}"
        );
        check!(
            error.message().contains("no service"),
            "the message is not friendly: {:?}",
            error.message()
        );
        check!(
            registry::stats().entries == 0,
            "a failed resolve left table entries"
        );
        Ok(())
    }

    /// A lease is a deadline: it survives before its tick and is pruned after
    /// it, without any owner action.
    pub fn lease_expiry_prunes() -> Result<(), String> {
        fresh()?;
        let (_service, callable) = channels::create().map_err(friendly)?;
        let published = handles::get(callable).map_err(friendly)?;
        let before = task::ticks();
        registry::register(
            task::KERNEL_TASK,
            "os.example.lease",
            published.kind,
            published.rights,
            published.object_id,
            &[],
            2,
        )
        .map_err(reason)?;
        check!(registry::stats().leases == 1, "the lease was not recorded");
        registry::prune_at(before);
        check!(
            registry::list().len() == 1,
            "the lease expired before its deadline"
        );
        check!(
            registry::prune_at(before + 100) == 1,
            "prune did not remove the expired name"
        );
        check!(
            registry::resolve(task::KERNEL_TASK, "os.example.lease")
                == Err(RegistryError::UnknownName),
            "the expired name still resolved"
        );
        check!(
            registry::stats().expirations == 1,
            "the expiration was not counted"
        );
        Ok(())
    }

    /// Owner death releases the name through both paths: the explicit
    /// `release_owner` teardown hook, and lazy pruning when a task dies without
    /// the hook running.
    pub fn owner_death_releases() -> Result<(), String> {
        fresh()?;
        let child = task::spawn_fork().map_err(to_string)?;

        // Path 1: the hook a task teardown calls.
        task::harness::switch_current(child);
        let (_service, callable) = channels::create().map_err(friendly)?;
        let published = handles::get(callable).map_err(friendly)?;
        registry::register(
            child,
            "os.example.hooked",
            published.kind,
            published.rights,
            published.object_id,
            &[1],
            0,
        )
        .map_err(reason)?;
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            registry::list().len() == 1,
            "the child's registration is missing"
        );
        check!(
            registry::release_owner(child) == 1,
            "release_owner did not drop the child's name"
        );
        check!(
            registry::resolve(task::KERNEL_TASK, "os.example.hooked")
                == Err(RegistryError::UnknownName),
            "the released name still resolved"
        );

        // Path 2: the owner dies without teardown; the next access prunes.
        task::harness::switch_current(child);
        let (_service2, callable2) = channels::create().map_err(friendly)?;
        let published2 = handles::get(callable2).map_err(friendly)?;
        registry::register(
            child,
            "os.example.crashed",
            published2.kind,
            published2.rights,
            published2.object_id,
            &[1],
            0,
        )
        .map_err(reason)?;
        task::harness::switch_current(task::KERNEL_TASK);
        task::harness::finish(child, 0);
        check!(
            registry::list().is_empty(),
            "prune kept a crashed owner's name"
        );
        check!(task::reap_child().is_some(), "the child was not reapable");
        Ok(())
    }

    /// The ACL hook gates a registry op before the table is touched: a policy
    /// that does not cover the caller denies the register and audits it.
    pub fn acl_denies_register() -> Result<(), String> {
        fresh()?;
        acl::load(&[acl::Rule {
            actor: 2000,
            interface_id: registry::INTERFACE,
            method: registry::method::REGISTER,
            allow: true,
        }]);
        credentials::set(
            task::KERNEL_TASK,
            credentials::Cred::new(1000, 100, 0, 0, 0),
        );
        in_space(|| -> Result<(), String> {
            let (_service, callable) = channels::create().map_err(friendly)?;
            let request = register_parcel("os.example.denied", callable, &[9], 0)?;
            write_bytes(REQUEST, &request);
            let before = audit::count();
            let args = MsgArgs {
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, result) = dispatch(OP_REGISTER, &args);
            check!(
                code == failed(errno::EACCES),
                "denied register -> {code:#x}"
            );
            check!(
                result.status == -errno::EACCES,
                "the denial status is {}",
                result.status
            );
            check!(
                registry::list().is_empty(),
                "a denied register touched the table"
            );
            check!(
                audit::count() == before + 1,
                "the denial was not audited: {} -> {}",
                before,
                audit::count()
            );
            let event = *audit::recent(1).first().ok_or("no audit event")?;
            check!(
                !event.allow
                    && event.interface_id == registry::INTERFACE
                    && event.method == registry::method::REGISTER,
                "the audit event is {event:?}"
            );
            Ok(())
        })
    }

    /// `list` reflects registrations, interfaces, owners and leases; a
    /// different owner cannot take a taken name, and an unrelated task without
    /// the admin capability cannot unregister it.
    pub fn list_reflects_state() -> Result<(), String> {
        fresh()?;
        let (_a, endpoint_a) = channels::create().map_err(friendly)?;
        let (_b, endpoint_b) = channels::create().map_err(friendly)?;
        let entry_a = handles::get(endpoint_a).map_err(friendly)?;
        let entry_b = handles::get(endpoint_b).map_err(friendly)?;
        registry::register(
            task::KERNEL_TASK,
            "os.example.alpha",
            entry_a.kind,
            entry_a.rights,
            entry_a.object_id,
            &[1, 2],
            100,
        )
        .map_err(reason)?;
        registry::register(
            task::KERNEL_TASK,
            "os.example.beta",
            entry_b.kind,
            entry_b.rights,
            entry_b.object_id,
            &[3],
            0,
        )
        .map_err(reason)?;

        let entries = registry::list();
        check!(entries.len() == 2, "list has {} entries", entries.len());
        check!(
            entries[0].name == "os.example.alpha" && entries[1].name == "os.example.beta",
            "list order is {:?}",
            entries.iter().map(|entry| &entry.name).collect::<Vec<_>>()
        );
        check!(
            entries[0].interfaces == vec![1, 2] && entries[1].interfaces == vec![3],
            "interfaces are {:?} / {:?}",
            entries[0].interfaces,
            entries[1].interfaces
        );
        check!(
            entries[0].owner_slot == task::KERNEL_TASK,
            "the owner is {}",
            entries[0].owner_slot
        );
        check!(
            entries[0].lease_remaining.is_some(),
            "alpha lost its lease in the listing"
        );
        check!(
            entries[1].lease_remaining.is_none(),
            "beta gained a lease in the listing"
        );
        let stats = registry::stats();
        check!(
            stats.entries == 2 && stats.leases == 1 && stats.registrations == 2,
            "registry stats are {stats:?}"
        );

        // A second owner cannot take the name; a stranger cannot withdraw it.
        let child = task::spawn_fork().map_err(to_string)?;
        credentials::set(child, credentials::Cred::new(1000, 100, 0, 0, 0));
        check!(
            registry::register(
                child,
                "os.example.alpha",
                entry_a.kind,
                entry_a.rights,
                entry_a.object_id,
                &[],
                0,
            ) == Err(RegistryError::NameTaken),
            "a second owner took a registered name"
        );
        check!(
            registry::unregister(child, task::KERNEL_TASK, "os.example.alpha")
                == Err(RegistryError::NotOwner),
            "a stranger unregistered a name"
        );

        // The owner withdraws one; state follows.
        registry::unregister(task::KERNEL_TASK, task::KERNEL_TASK, "os.example.alpha")
            .map_err(reason)?;
        let entries = registry::list();
        check!(
            entries.len() == 1 && entries[0].name == "os.example.beta",
            "list after unregister is {:?}",
            entries.iter().map(|entry| &entry.name).collect::<Vec<_>>()
        );
        check!(
            registry::stats().unregistrations == 1,
            "the unregistration was not counted"
        );
        registry::unregister(task::KERNEL_TASK, task::KERNEL_TASK, "os.example.beta")
            .map_err(reason)?;
        check!(registry::list().is_empty(), "the table is not empty");
        Ok(())
    }

    /// The `messengerd` proxy path: a task holding `CAP_IPC_CONTROL` names
    /// another task's slot as the target, so the kernel reads the client's
    /// endpoint handle from the client's table, records the client as owner,
    /// and opens a resolved handle back into the client. Without the
    /// capability the same request is refused.
    pub fn proxy_registers_for_client() -> Result<(), String> {
        fresh()?;
        let client = task::spawn_fork().map_err(to_string)?;

        // The client owns a channel and keeps the receiving side.
        task::harness::switch_current(client);
        let (_service, callable) = channels::create().map_err(friendly)?;
        task::harness::switch_current(task::KERNEL_TASK);
        let published = handles::get_for_task(client, callable).map_err(friendly)?;

        in_space(|| -> Result<(), String> {
            // Register with the client's slot as target: the proxy is the
            // caller, but the name must belong to the client.
            let request = register_parcel("os.example.proxy", callable, &[5], 0)?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                txn_id: client as u64,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, _) = dispatch(OP_REGISTER, &args);
            check!(code == 0, "proxied register -> {code:#x}");
            let entries = registry::list();
            check!(
                entries.len() == 1 && entries[0].owner_slot == client,
                "the owner is not the client: {:?}",
                entries
            );

            // Resolve with the client's slot as target: the handle must land in
            // the *client's* table, not the proxy's.
            let request = string_parcel(registry::method::RESOLVE, "os.example.proxy")?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                txn_id: client as u64,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, result) = dispatch(OP_RESOLVE, &args);
            check!(code == 0, "proxied resolve -> {code:#x}");
            let resolved = handles::get_for_task(client, result.value).map_err(friendly)?;
            check!(
                resolved.object_id == published.object_id,
                "the proxied handle names a different object"
            );
            check!(
                handles::count_for_task(client) == 3,
                "the client holds {} handles, expected 3",
                handles::count_for_task(client)
            );

            // Without the capability the same target is refused.
            credentials::set(
                task::KERNEL_TASK,
                credentials::Cred::new(1000, 100, 0, 0, 0),
            );
            let request = string_parcel(registry::method::RESOLVE, "os.example.proxy")?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                txn_id: client as u64,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, _) = dispatch(OP_RESOLVE, &args);
            check!(
                code == failed(errno::EPERM),
                "an unprivileged proxy -> {code:#x}"
            );

            // Unregister through the proxy: the capability authorises, but the
            // name still belongs to the client, so the client's slot must stay
            // the owner named by the request.
            credentials::reset_for_task(task::KERNEL_TASK);
            let request = string_parcel(registry::method::UNREGISTER, "os.example.proxy")?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                txn_id: client as u64,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, _) = dispatch(OP_UNREGISTER, &args);
            check!(code == 0, "proxied unregister -> {code:#x}");
            check!(registry::list().is_empty(), "the table is not empty");
            Ok(())
        })?;

        task::harness::finish(client, 0);
        check!(task::reap_child().is_some(), "the client was not reapable");
        Ok(())
    }

    /// The ops over the native gate: register, resolve, list, unknown-name and
    /// unregister all round-trip through the ABI blocks and the TLV bodies.
    pub fn syscall_roundtrip() -> Result<(), String> {
        fresh()?;
        in_space(|| -> Result<(), String> {
            let (_service, callable) = channels::create().map_err(friendly)?;
            let published = handles::get(callable).map_err(friendly)?;

            // Register through OP_REGISTER; the reply names the object.
            let request = register_parcel("os.example.sys", callable, &[7, 8], 0)?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, result) = dispatch(OP_REGISTER, &args);
            check!(code == 0, "register -> {code:#x}");
            check!(
                result.value == published.object_id,
                "register returned object {} (expected {})",
                result.value,
                published.object_id
            );

            // Resolve through OP_RESOLVE duplicates the handle for this task.
            let request = string_parcel(registry::method::RESOLVE, "os.example.sys")?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, result) = dispatch(OP_RESOLVE, &args);
            check!(code == 0, "resolve -> {code:#x}");
            check!(
                result.value != callable,
                "resolve reused the registered handle"
            );
            let copy = handles::get(result.value).map_err(friendly)?;
            check!(
                copy.object_id == published.object_id,
                "the resolved handle names a different object"
            );

            // List through OP_LIST writes an encoded parcel of records.
            let args = MsgArgs {
                buf_ptr: LIST_BUF,
                buf_cap: 4096,
                ..MsgArgs::default()
            };
            let (code, result) = dispatch(OP_LIST, &args);
            check!(code == 0, "list -> {code:#x}");
            let bytes = read_bytes(LIST_BUF, result.bytes as usize);
            let parcel = Parcel::decode(&bytes).map_err(friendly)?;
            check!(
                parcel.header.interface_id == registry::INTERFACE
                    && parcel.header.method == registry::method::LIST,
                "the list parcel header is {:?}",
                parcel.header
            );
            let mut decoder = Decoder::new(&parcel.body);
            let mut records = 0;
            while let Some(field) = decoder.next().map_err(friendly)? {
                if field.kind == Kind::Struct && field.id == registry::field::ENTRY {
                    records += 1;
                }
            }
            check!(records == 1, "the list body has {records} records");

            // An unknown name is a friendly -ENOENT.
            let request = string_parcel(registry::method::RESOLVE, "no.such.service")?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, result) = dispatch(OP_RESOLVE, &args);
            check!(
                code == failed(errno::ENOENT),
                "unknown resolve -> {code:#x}"
            );
            check!(
                result.status == -errno::ENOENT,
                "the unknown-name status is {}",
                result.status
            );

            // Unregister through OP_UNREGISTER empties the table.
            let request = string_parcel(registry::method::UNREGISTER, "os.example.sys")?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, _) = dispatch(OP_UNREGISTER, &args);
            check!(code == 0, "unregister -> {code:#x}");
            check!(registry::list().is_empty(), "the table is not empty");
            Ok(())
        })
    }
}

/// Topic ACL hooks (issue #92): stable segment hashes, per-segment policy,
/// wildcard filters, and the `authorize_topic` syscall gate. The broker itself
/// is userspace; these tests pin the kernel half of the contract.
mod topics_suite {
    use super::*;
    use crate::ipc::credentials::{self, Cred};
    use crate::ipc::syscalls::{
        errno, MsgArgs, MsgResult, OP_AUTHORIZE_TOPIC, REGISTRY_TARGET_SELF,
    };
    use crate::ipc::{acl, audit, topics};
    use libmessenger::{Encoder, Header, Parcel, VERSION};

    /// Scratch user address space for the syscall-level test: `dispatch`
    /// validates pointers against the active CR3.
    const SPACE: u64 = 0x0040_0000;
    const SPACE_PAGES: u64 = 4;
    const ARGS: u64 = SPACE;
    const RESULT: u64 = SPACE + 0x100;
    const REQUEST: u64 = SPACE + 0x1000;

    /// Every topics test starts from the bring-up state: kernel task current,
    /// root credentials, empty policy (the bootstrap window), empty audit ring.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        credentials::reset_for_task(task::KERNEL_TASK);
        acl::load(&[]);
        audit::reset();
        audit::set_trace(false);
        task::wake_task(task::KERNEL_TASK);
        let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
        Ok(())
    }

    /// Run `f` with [`SPACE`] mapped into a fresh address space installed as
    /// CR3, exactly as a real syscall from a user task would find it.
    fn in_space<R>(f: impl FnOnce() -> Result<R, String>) -> Result<R, String> {
        let kernel = mem::kernel_table();
        let table = mem::new_user_table().ok_or("new_user_table failed")?;
        process::map_range(table, SPACE, SPACE + SPACE_PAGES * 4096).map_err(to_string)?;
        mem::switch_to(table);
        let outcome = f();
        mem::switch_to(kernel);
        mem::free_user_table(table);
        outcome
    }

    fn write_bytes(va: u64, bytes: &[u8]) {
        // Safety: the scratch pages are mapped writable while installed.
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), va as *mut u8, bytes.len()) };
    }

    fn read_bytes(va: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        out.resize(len, 0);
        // Safety: the scratch pages are mapped readable while installed.
        unsafe { core::ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), len) };
        out
    }

    /// Run one op through the native gate with the args block at [`ARGS`].
    fn dispatch(op: u64, args: &MsgArgs) -> (u64, MsgResult) {
        write_bytes(ARGS, &args.to_bytes());
        let code = process::dispatch_for_test(5, op, ARGS, RESULT);
        let result = MsgResult::from_bytes(&read_bytes(RESULT, 64))
            .expect("the kernel wrote a malformed result block");
        (code, result)
    }

    /// Two's-complement `-errno` as the syscall returns it in `rax`.
    fn failed(code: i64) -> u64 {
        (code as u64).wrapping_neg()
    }

    /// Encode an `authorize_topic` request parcel.
    fn auth_parcel(name: &str, mode: u32, txn: u64) -> Result<Vec<u8>, String> {
        let mut body = Encoder::new();
        body.string(topics::field::NAME, name)
            .map_err(|error| error.message())?;
        body.u32(topics::field::MODE, mode)
            .map_err(|error| error.message())?;
        body.u64(topics::field::TXN, txn)
            .map_err(|error| error.message())?;
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: 0,
                interface_id: 0,
                method: 0,
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

    /// A policy that denies `method` on `interface` for uid 1000 ahead of an
    /// allow-all rule, so neighbours and other actors stay allowed.
    fn deny_rule(interface: u64, method: u32) -> [acl::Rule; 2] {
        [
            acl::Rule {
                actor: 1000,
                interface_id: interface,
                method,
                allow: false,
            },
            acl::Rule {
                actor: acl::ANY_ACTOR,
                interface_id: acl::ANY_INTERFACE,
                method: acl::ANY_METHOD,
                allow: true,
            },
        ]
    }

    /// The interface ids and segment methods are derived, not guessed: the
    /// constants must keep matching the documented names, and the validation
    /// rules must match the userspace broker.
    pub fn segment_methods_stable() -> Result<(), String> {
        check!(
            topics::fnv1a64("os.lazy.messenger.topics.publish.v1") == topics::PUBLISH_INTERFACE,
            "the publish interface constant drifted from its name"
        );
        check!(
            topics::fnv1a64("os.lazy.messenger.topics.subscribe.v1") == topics::SUBSCRIBE_INTERFACE,
            "the subscribe interface constant drifted from its name"
        );
        check!(
            topics::segment_method("system") == 1_226_705_564,
            "the segment hash for `system` changed: {}",
            topics::segment_method("system")
        );
        check!(
            topics::segment_method("+") != topics::segment_method("#")
                && topics::segment_method("+") != 0,
            "wildcard segment ids are not distinct"
        );
        check!(
            topics::interface(topics::MODE_SUBSCRIBE) == Some(topics::SUBSCRIBE_INTERFACE)
                && topics::interface(99).is_none(),
            "the mode-to-interface map is wrong"
        );

        check!(
            topics::validate("system/events/network/up", topics::MODE_PUBLISH) == Ok(4),
            "a literal four-segment topic was rejected"
        );
        check!(
            topics::validate("system/+", topics::MODE_PUBLISH) == Err(topics::Error::BadName),
            "publish accepted a wildcard"
        );
        check!(
            topics::validate("system/+", topics::MODE_SUBSCRIBE) == Ok(2),
            "subscribe rejected a `+` segment"
        );
        check!(
            topics::validate("system/#", topics::MODE_SUBSCRIBE) == Ok(2),
            "subscribe rejected a trailing `#`"
        );
        check!(
            topics::validate("system/#/up", topics::MODE_SUBSCRIBE) == Err(topics::Error::BadName),
            "subscribe accepted a non-trailing `#`"
        );
        check!(
            topics::validate("", topics::MODE_PUBLISH) == Err(topics::Error::BadName)
                && topics::validate("a//b", topics::MODE_PUBLISH) == Err(topics::Error::BadName),
            "empty segments were accepted"
        );
        check!(
            topics::validate("a/b/c/d/e/f/g/h/i", topics::MODE_PUBLISH)
                == Err(topics::Error::BadName),
            "a nine-segment topic was accepted"
        );
        check!(
            topics::validate("system events", topics::MODE_PUBLISH) == Err(topics::Error::BadName),
            "a topic with a space was accepted"
        );
        Ok(())
    }

    /// Policy is evaluated per segment: one denied segment blocks the whole
    /// name, its neighbours pass, and the denial lands in the audit ring with
    /// the broker's correlation id.
    pub fn acl_segments_enforced() -> Result<(), String> {
        fresh()?;
        let slot = task::current();
        credentials::set(slot, Cred::new(1000, 100, 0, 0, 0));
        acl::load(&deny_rule(
            topics::PUBLISH_INTERFACE,
            topics::segment_method("secret"),
        ));

        let allowed = topics::authorize(slot, topics::MODE_PUBLISH, "public/data", 0x11)
            .map_err(|error| String::from(error.message()))?;
        check!(
            allowed == 2,
            "the allowed topic reported {allowed} segments"
        );

        let count_before = audit::count();
        let denied = topics::authorize(slot, topics::MODE_PUBLISH, "public/secret/data", 0xabc);
        check!(
            denied == Err(topics::Error::Denied),
            "a denied middle segment was allowed: {denied:?}"
        );
        check!(
            audit::count() == count_before + 1,
            "the denial did not reach the audit ring"
        );
        let event = *audit::recent(1)
            .first()
            .ok_or("the denial left no audit event")?;
        check!(
            event.uid == 1000
                && event.interface_id == topics::PUBLISH_INTERFACE
                && event.method == topics::segment_method("secret")
                && event.txn_id == 0xabc
                && !event.allow,
            "the audit event lost the topic segment or actor: {event:?}"
        );

        // A publish-mode rule must not leak into subscribe mode.
        check!(
            topics::authorize(slot, topics::MODE_SUBSCRIBE, "public/secret/data", 0).is_ok(),
            "the publish deny leaked into subscribe"
        );

        // Bad names and modes never touch policy.
        check!(
            topics::authorize(slot, topics::MODE_PUBLISH, "public/+", 0)
                == Err(topics::Error::BadName),
            "a publish wildcard reached policy"
        );
        check!(
            topics::authorize(slot, 9, "public/data", 0) == Err(topics::Error::BadMode),
            "an unknown mode reached policy"
        );
        Ok(())
    }

    /// Wildcard subscription segments are policy-checked like literals, so a
    /// filter cannot bypass a namespace rule.
    pub fn acl_wildcard_filter() -> Result<(), String> {
        fresh()?;
        let slot = task::current();
        credentials::set(slot, Cred::new(1000, 100, 0, 0, 0));
        acl::load(&deny_rule(
            topics::SUBSCRIBE_INTERFACE,
            topics::segment_method("#"),
        ));
        check!(
            topics::authorize(slot, topics::MODE_SUBSCRIBE, "system/#", 0)
                == Err(topics::Error::Denied),
            "a `#` filter bypassed the wildcard deny"
        );
        check!(
            topics::authorize(slot, topics::MODE_SUBSCRIBE, "system/events", 0).is_ok(),
            "the literal prefix was denied with the wildcard"
        );

        // `+` is a distinct method id and can be denied on its own.
        acl::load(&deny_rule(
            topics::SUBSCRIBE_INTERFACE,
            topics::segment_method("+"),
        ));
        check!(
            topics::authorize(slot, topics::MODE_SUBSCRIBE, "system/+/up", 0)
                == Err(topics::Error::Denied),
            "a `+` filter bypassed the wildcard deny"
        );
        check!(
            topics::authorize(slot, topics::MODE_SUBSCRIBE, "system/up", 0).is_ok(),
            "`+` denied a literal neighbour"
        );
        Ok(())
    }

    /// The syscall gate: allow returns the segment count, deny is `-EACCES`
    /// with an audit record, proxy targets require `CAP_IPC_CONTROL`, and
    /// malformed requests are `-EINVAL`.
    pub fn syscall_gate() -> Result<(), String> {
        fresh()?;
        in_space(|| -> Result<(), String> {
            // Empty policy (the bootstrap window): the whole topic is allowed
            // and the op reports how many segments it checked.
            let request = auth_parcel("system/events", topics::MODE_PUBLISH, 0x5)?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                txn_id: REGISTRY_TARGET_SELF,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, result) = dispatch(OP_AUTHORIZE_TOPIC, &args);
            check!(code == 0, "authorize -> {code:#x}");
            check!(
                result.value == 2,
                "the gate reported {} segments",
                result.value
            );

            // A deny rule for one segment turns the whole request into -EACCES
            // and records the denial (with the request's correlation id). The
            // rule keys on uid 1000, so drop the caller's root identity.
            credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
            acl::load(&deny_rule(
                topics::PUBLISH_INTERFACE,
                topics::segment_method("secret"),
            ));
            let count_before = audit::count();
            let request = auth_parcel("secret/data", topics::MODE_PUBLISH, 0x77)?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                txn_id: REGISTRY_TARGET_SELF,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, result) = dispatch(OP_AUTHORIZE_TOPIC, &args);
            check!(
                code == failed(errno::EACCES),
                "denied authorize -> {code:#x}"
            );
            check!(
                result.status == -errno::EACCES,
                "the denial status is {}",
                result.status
            );
            check!(
                audit::count() == count_before + 1,
                "the syscall denial was not audited"
            );

            // A proxy target needs CAP_IPC_CONTROL. The kernel task is root
            // with every cap by default, so drop to an unprivileged identity.
            credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
            let request = auth_parcel("system/events", topics::MODE_PUBLISH, 0)?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                txn_id: 1,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, _) = dispatch(OP_AUTHORIZE_TOPIC, &args);
            check!(
                code == failed(errno::EPERM),
                "proxy without cap -> {code:#x}"
            );

            // With the cap the same call evaluates the other actor (root, so
            // the deny rule above does not match it).
            credentials::set(
                task::current(),
                Cred::new(1000, 100, credentials::CAP_IPC_CONTROL, 0, 0),
            );
            let args = MsgArgs {
                txn_id: 1,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, result) = dispatch(OP_AUTHORIZE_TOPIC, &args);
            check!(code == 0, "proxy with cap -> {code:#x}");
            check!(
                result.value == 2,
                "the proxy checked {} segments",
                result.value
            );

            // Malformed: publish wildcard, unknown mode, and an empty body.
            let request = auth_parcel("system/+", topics::MODE_PUBLISH, 0)?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                txn_id: REGISTRY_TARGET_SELF,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, _) = dispatch(OP_AUTHORIZE_TOPIC, &args);
            check!(
                code == failed(errno::EINVAL),
                "publish wildcard -> {code:#x}"
            );

            let request = auth_parcel("system/events", 9, 0)?;
            write_bytes(REQUEST, &request);
            let args = MsgArgs {
                txn_id: REGISTRY_TARGET_SELF,
                parcel_ptr: REQUEST,
                parcel_len: request.len() as u64,
                ..MsgArgs::default()
            };
            let (code, _) = dispatch(OP_AUTHORIZE_TOPIC, &args);
            check!(code == failed(errno::EINVAL), "unknown mode -> {code:#x}");

            let args = MsgArgs {
                txn_id: REGISTRY_TARGET_SELF,
                ..MsgArgs::default()
            };
            let (code, _) = dispatch(OP_AUTHORIZE_TOPIC, &args);
            check!(code == failed(errno::E2BIG), "empty request -> {code:#x}");
            Ok(())
        })
    }
}
