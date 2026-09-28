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
    (
        "quota_charge_release_accounting",
        quota_suite::charge_release_accounting,
    ),
    (
        "quota_denial_friendly_error",
        quota_suite::denial_friendly_error,
    ),
    (
        "quota_release_restores_headroom",
        quota_suite::release_restores_headroom,
    ),
    (
        "quota_per_uid_aggregation",
        quota_suite::per_uid_aggregation,
    ),
    (
        "quota_syscall_introspection",
        quota_suite::syscall_introspection,
    ),
    (
        "quota_shared_buffer_charge",
        quota_suite::shared_buffer_charge,
    ),
    (
        "quota_channel_queue_charge",
        quota_suite::channel_queue_charge,
    ),
    ("task_kernel_registered", task_suite::kernel_registered),
    (
        "task_block_wake_roundtrip",
        task_suite::block_wake_roundtrip,
    ),
    ("task_fork_reap_churn", task_suite::fork_reap_churn),
    ("task_thread_exit_reclaim", task_suite::thread_exit_reclaim),
    (
        "task_thread_churn_generations",
        task_suite::thread_churn_generations,
    ),
    (
        "task_soak_thread_exit_generations",
        task_suite::soak_thread_exit_generations,
    ),
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
        "task_snapshot_matches_process_list",
        task_suite::task_snapshot_matches_process_list,
    ),
    (
        "task_snapshot_soak_fork_churn",
        task_suite::task_snapshot_soak_fork_churn,
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
    (
        "task_signal_linux_sigset_roundtrip",
        signal_suite::linux_sigset_roundtrip,
    ),
    (
        "task_signal_linux_sigprocmask_boundary",
        signal_suite::linux_sigprocmask_sigset_boundary,
    ),
    (
        "task_signal_linux_sigset_soak",
        signal_suite::linux_sigset_translate_soak,
    ),
    ("task_signal_stop_continue", signal_suite::stop_continue),
    (
        "pipe_syscalls_create_and_io",
        pipe_suite::syscalls_create_and_io,
    ),
    ("pipe_ring_wrap_roundtrip", pipe_suite::ring_wrap_roundtrip),
    (
        "pipe_blocking_read_write_wake",
        pipe_suite::blocking_read_write_wake,
    ),
    ("pipe_eof_epipe_nonblock", pipe_suite::eof_epipe_nonblock),
    ("pipe_dup_fork_cloexec", pipe_suite::dup_fork_cloexec),
    ("pipe_vfork_clone_child", pipe_suite::vfork_clone_child),
    (
        "pipe_soak_throughput_and_lifecycle",
        pipe_suite::soak_throughput_and_lifecycle,
    ),
    (
        "linux_mremap_grow_shrink_move",
        linux_suite::mremap_grow_shrink_move,
    ),
    ("linux_mremap_soak_churn", linux_suite::mremap_soak_churn),
    ("linux_eventfd_semantics", linux_suite::eventfd_semantics),
    (
        "linux_epoll_level_edge_hangup",
        linux_suite::epoll_level_edge_hangup,
    ),
    (
        "linux_epoll_edge_over_maxevents",
        linux_suite::epoll_edge_over_maxevents,
    ),
    (
        "linux_epoll_level_does_not_starve",
        linux_suite::epoll_level_does_not_starve,
    ),
    (
        "linux_epoll_soak_add_wait_cycles",
        linux_suite::epoll_soak_add_wait_cycles,
    ),
    (
        "linux_seqpacket_boundaries",
        linux_suite::seqpacket_boundaries,
    ),
    (
        "linux_seqpacket_soak_messages",
        linux_suite::seqpacket_soak_messages,
    ),
    (
        "linux_unix_pair_eof_shutdown",
        linux_suite::unix_pair_eof_shutdown,
    ),
    (
        "linux_unix_pathname_bind_connect_accept",
        linux_suite::unix_pathname_bind_connect_accept,
    ),
    (
        "linux_unix_write_before_accept",
        linux_suite::unix_write_before_accept,
    ),
    ("linux_unix_pathname_soak", linux_suite::unix_pathname_soak),
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
    (
        "ipc_channel_concurrent_clients_allowed",
        ipc_channel_suite::concurrent_clients_allowed,
    ),
    (
        "ipc_channel_concurrent_clients_soak",
        ipc_channel_suite::concurrent_clients_soak,
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
        "ipc_credentials_transition_requires_cap",
        credentials_suite::transition_requires_cap,
    ),
    (
        "ipc_credentials_transition_rejects_widening",
        credentials_suite::transition_rejects_widening,
    ),
    (
        "ipc_credentials_syscall_gate",
        credentials_suite::syscall_gate,
    ),
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
        "block_fake_read_write_flush",
        block_suite::fake_read_write_flush,
    ),
    (
        "block_registry_register_lookup_duplicate",
        block_suite::registry_register_lookup_duplicate,
    ),
    ("block_ata_reads_fat_root", block_suite::ata_reads_fat_root),
    (
        "fs_path_resolution_and_mounts",
        fs_suite::path_resolution_and_mounts,
    ),
    (
        "fs_ramfs_create_write_read_rename_unlink",
        fs_suite::ramfs_create_write_read_rename_unlink,
    ),
    (
        "fs_permission_matrix_owner_group_other",
        fs_suite::permission_matrix_owner_group_other,
    ),
    (
        "fs_traversal_and_sticky_bits",
        fs_suite::traversal_and_sticky_bits,
    ),
    ("fs_cache_invalidation", fs_suite::cache_invalidation),
    ("fs_fat_read_only_erofs", fs_suite::fat_read_only_erofs),
    (
        "fs_getdents64_ramfs_directory",
        fs_suite::getdents64_ramfs_directory,
    ),
    (
        "fs_overlay_copy_up_read_write",
        overlay_suite::copy_up_read_write,
    ),
    (
        "fs_overlay_dir_create_remove",
        overlay_suite::dir_create_remove,
    ),
    ("fs_overlay_rename_replace", overlay_suite::rename_replace),
    ("fs_overlay_enospc_limits", overlay_suite::enospc_limits),
    (
        "fs_overlay_soak_generations",
        overlay_suite::soak_generations,
    ),
    ("fs_abi_mkdir_rename_rmdir", overlay_suite::abi_syscalls),
    ("fs_abi_unlink_while_open", overlay_suite::unlink_while_open),
    (
        "fs_ext2_create_write_read_rename_unlink",
        ext2_suite::create_write_read_rename_unlink,
    ),
    ("fs_ext2_block_sizes", ext2_suite::block_sizes),
    ("fs_ext2_rejects_corruption", ext2_suite::rejects_corruption),
    (
        "fs_ext2_mount_device_wiring",
        ext2_suite::mount_device_wiring,
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
    (
        "service_spawn_child_parent_and_wait",
        service_suite::spawn_child_parent_and_wait,
    ),
    (
        "service_spawn_child_rejects_bad_image",
        service_suite::spawn_child_rejects_bad_image,
    ),
    (
        "service_spawn_unknown_file_fails",
        service_suite::spawn_unknown_file_fails,
    ),
    (
        "keyd_sha256_hmac_known_answers",
        crypto_suite::sha256_hmac_known_answers,
    ),
    (
        "keyd_wrap_roundtrip_share_only",
        crypto_suite::keyd_wrap_roundtrip_share_only,
    ),
    (
        "display_kernel_bind_refused",
        display_suite::kernel_bind_refused,
    ),
    (
        "display_bind_input_present_roundtrip",
        display_suite::bind_input_present_roundtrip,
    ),
    (
        "sysinfo_snapshot_abi_contract",
        sysinfo_suite::snapshot_abi_contract,
    ),
    (
        "sysinfo_snapshot_rejects_bad_destinations",
        sysinfo_suite::snapshot_rejects_bad_destinations,
    ),
    (
        "sysinfo_snapshot_fields_sane",
        sysinfo_suite::snapshot_fields_sane,
    ),
    (
        "sysinfo_snapshot_reflects_spawned_task",
        sysinfo_suite::snapshot_reflects_spawned_task,
    ),
    (
        "sysinfo_soak_snapshot_task_churn",
        sysinfo_suite::soak_snapshot_task_churn,
    ),
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
// Per-uid resource quotas (issue #103)
// ---------------------------------------------------------------------------

mod quota_suite {
    use super::*;
    use crate::ipc::channels;
    use crate::ipc::credentials::{self, Cred};
    use crate::ipc::handles::{self, rights, Error as HandleError, HandleKind};
    use crate::ipc::shared;
    use crate::quota::{self, Resource};
    use libmessenger::{flags, Encoder, Header, Parcel, VERSION};

    /// Every quota test starts from an empty ledger, root credentials, empty
    /// registries, and a runnable kernel task.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        task::harness::reset();
        channels::reset();
        shared::reset();
        handles::reset_for_task(task::current());
        credentials::reset_for_task(task::current());
        quota::reset();
        Ok(())
    }

    /// Friendly-message adapters for `Result` plumbing.
    fn handle_reason(error: HandleError) -> String {
        error.message().into()
    }

    fn channel_reason(error: channels::Error) -> String {
        error.message().into()
    }

    /// A minimal one-way parcel for the channel choke-point test.
    fn one_way() -> Result<Vec<u8>, String> {
        let mut body = Encoder::new();
        body.u64(1, 7).map_err(|error| error.message())?;
        let parcel = Parcel {
            header: Header {
                version: VERSION,
                flags: flags::ONE_WAY,
                interface_id: 0x0102_0304,
                method: 1,
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

    /// Charges accumulate, record a peak and counters, and releases give the
    /// usage back; an unseen uid reads the documented default table.
    pub fn charge_release_accounting() -> Result<(), String> {
        fresh()?;
        let uid = 4242;
        let resource = Resource::KernelMemory;
        quota::set_limit(uid, resource, 4096);
        check!(
            quota::usage(uid, resource) == 0,
            "a fresh ledger has nonzero usage"
        );
        check!(
            quota::limit(uid, resource) == 4096,
            "set_limit did not stick: {}",
            quota::limit(uid, resource)
        );
        quota::charge(uid, resource, 1000).map_err(|error| error.message())?;
        quota::charge(uid, resource, 24).map_err(|error| error.message())?;
        check!(
            quota::usage(uid, resource) == 1024,
            "usage is {}, expected 1024",
            quota::usage(uid, resource)
        );
        let stats = quota::stats(uid);
        check!(
            stats.peak[resource.index()] == 1024,
            "peak is {}, expected 1024",
            stats.peak[resource.index()]
        );
        check!(
            stats.charges == 2,
            "charges are {}, expected 2",
            stats.charges
        );
        quota::release(uid, resource, 1000);
        check!(
            quota::usage(uid, resource) == 24,
            "release left {}, expected 24",
            quota::usage(uid, resource)
        );
        check!(
            quota::stats(uid).releases == 1,
            "the release was not counted"
        );

        // A uid with no ledger reads defaults; root reads the uncapped table.
        check!(
            quota::usage(9999, Resource::Handles) == 0,
            "an unseen uid has usage"
        );
        check!(
            quota::limit(9999, Resource::Handles)
                == quota::DEFAULT_LIMITS[Resource::Handles.index()],
            "an unseen uid did not read the default handle limit"
        );
        check!(
            quota::limit(0, Resource::Handles) == quota::ROOT_LIMITS[Resource::Handles.index()],
            "root is not uncapped"
        );
        quota::reset();
        Ok(())
    }

    /// A charge over the limit is refused with a friendly error that names the
    /// resource and carries the usage and limit; the boundary is exact.
    pub fn denial_friendly_error() -> Result<(), String> {
        fresh()?;
        let uid = 5150;
        let resource = Resource::QueueBytes;
        quota::set_limit(uid, resource, 256);
        quota::charge(uid, resource, 200).map_err(|error| error.message())?;

        let error = quota::charge(uid, resource, 100).expect_err("an over-limit charge succeeded");
        check!(
            error.uid == uid
                && error.resource == resource
                && error.usage == 200
                && error.limit == 256,
            "the refusal lost its numbers: {error:?}"
        );
        let text = error.message();
        check!(text.contains("uid 5150"), "message lost the uid: {text}");
        check!(
            text.contains("200") && text.contains("256"),
            "message lost the usage/limit: {text}"
        );
        check!(
            text.contains("quota") && text.contains("bytes"),
            "message is not a friendly quota error: {text}"
        );
        check!(
            quota::usage(uid, resource) == 200,
            "a refused charge changed usage"
        );
        check!(
            quota::stats(uid).denials == 1,
            "the refusal was not counted"
        );

        // Exactly to the limit fits; one byte more does not.
        check!(
            quota::check(uid, resource, 56).is_ok(),
            "the boundary refused a fitting charge"
        );
        check!(
            quota::check(uid, resource, 57).is_err(),
            "the boundary allowed an over-limit charge"
        );
        quota::reset();
        Ok(())
    }

    /// A denied charge leaves usage alone; releasing part of it restores
    /// headroom so the same amount fits again.
    pub fn release_restores_headroom() -> Result<(), String> {
        fresh()?;
        let uid = 6262;
        let resource = Resource::Handles;
        quota::set_limit(uid, resource, 100);
        quota::charge(uid, resource, 100).map_err(|error| error.message())?;
        check!(
            quota::charge(uid, resource, 1).is_err(),
            "the limit was not enforced"
        );
        quota::release(uid, resource, 40);
        quota::charge(uid, resource, 40).map_err(|error| error.message())?;
        check!(
            quota::usage(uid, resource) == 100,
            "usage after release+recharge is {}",
            quota::usage(uid, resource)
        );
        check!(
            quota::charge(uid, resource, 1).is_err(),
            "headroom was restored beyond the limit"
        );
        check!(
            quota::stats(uid).over_releases == 0,
            "balanced releases reported an over-release"
        );
        quota::reset();
        Ok(())
    }

    /// Two tasks of one uid share a single per-uid handle limit through the
    /// handle-table choke point, and teardown of either gives headroom back.
    pub fn per_uid_aggregation() -> Result<(), String> {
        fresh()?;
        let uid = 7373;
        let first = task::MAX_TASKS - 1;
        let second = task::MAX_TASKS - 2;
        quota::set_limit(uid, Resource::Handles, 2);
        credentials::set(first, Cred::new(uid, 0, 0, 0, 0));
        credentials::set(second, Cred::new(uid, 0, 0, 0, 0));
        handles::reset_for_task(first);
        handles::reset_for_task(second);

        handles::open_for_task(first, HandleKind::Object, rights::CALL, 11)
            .map_err(handle_reason)?;
        check!(
            quota::usage(uid, Resource::Handles) == 1,
            "the first task's handle was not charged"
        );
        handles::open_for_task(second, HandleKind::Object, rights::CALL, 12)
            .map_err(handle_reason)?;
        check!(
            quota::usage(uid, Resource::Handles) == 2,
            "the two tasks did not aggregate: {}",
            quota::usage(uid, Resource::Handles)
        );

        // Each table still has room (MAX_HANDLES); the shared uid limit is what
        // refuses the third handle.
        check!(
            handles::open_for_task(first, HandleKind::Object, rights::CALL, 13)
                == Err(HandleError::Quota),
            "the per-uid aggregate handle limit was not enforced"
        );
        check!(
            quota::usage(uid, Resource::Handles) == 2,
            "a refused open leaked usage"
        );

        // Resetting one task's table releases only its handles.
        handles::reset_for_task(first);
        check!(
            quota::usage(uid, Resource::Handles) == 1,
            "teardown released the wrong number of handles"
        );
        handles::reset_for_task(second);
        check!(
            quota::usage(uid, Resource::Handles) == 0,
            "teardown stranded uid usage"
        );
        credentials::reset_for_task(first);
        credentials::reset_for_task(second);
        quota::reset();
        Ok(())
    }

    /// Native syscall 11 copies the caller's usage/limits block in resource
    /// order and refuses a null pointer.
    pub fn syscall_introspection() -> Result<(), String> {
        fresh()?;
        let uid = credentials::of(task::current()).uid;
        quota::set_limit(uid, Resource::QueueDepth, 77);
        quota::charge(uid, Resource::QueueDepth, 5).map_err(|error| error.message())?;

        let mut words = [0u64; quota::STATS_WORDS];
        let code = process::dispatch_for_test(11, words.as_mut_ptr() as u64, 0, 0);
        check!(code == 0, "quota syscall returned {code:#x}");
        for resource in Resource::ALL {
            let index = resource.index();
            check!(
                words[index * 2] == quota::usage(uid, resource),
                "usage word for {resource:?} is {}, expected {}",
                words[index * 2],
                quota::usage(uid, resource)
            );
            check!(
                words[index * 2 + 1] == quota::limit(uid, resource),
                "limit word for {resource:?} is {}, expected {}",
                words[index * 2 + 1],
                quota::limit(uid, resource)
            );
        }
        check!(
            words[Resource::QueueDepth.index() * 2] == 5,
            "queued usage word is {}, expected 5",
            words[Resource::QueueDepth.index() * 2]
        );
        check!(
            words[Resource::QueueDepth.index() * 2 + 1] == 77,
            "queued limit word is {}, expected 77",
            words[Resource::QueueDepth.index() * 2 + 1]
        );
        check!(
            process::dispatch_for_test(11, 0, 0, 0) == 0u64.wrapping_sub(14),
            "a null stats buffer was not refused with -EFAULT"
        );
        quota::reset();
        Ok(())
    }

    /// A shared buffer charges its frames to the creator's uid as kernel
    /// memory, and closing it gives the charge back.
    pub fn shared_buffer_charge() -> Result<(), String> {
        fresh()?;
        let uid = credentials::of(task::current()).uid;
        let resource = Resource::KernelMemory;
        let handle = shared::create(2 * 4096, shared::flags::READ | shared::flags::WRITE)
            .map_err(|error| error.message())?;
        let info = shared::info(handle).map_err(|error| error.message())?;
        check!(
            quota::usage(uid, resource) == info.size,
            "kernel memory usage is {}, buffer is {} bytes",
            quota::usage(uid, resource),
            info.size
        );
        shared::close(handle).map_err(|error| error.message())?;
        check!(
            quota::usage(uid, resource) == 0,
            "closing the buffer stranded {} bytes",
            quota::usage(uid, resource)
        );
        fresh()
    }

    /// Queueing a message charges its sender's uid for both depth and bytes,
    /// the next message is refused at the depth limit, and delivery releases
    /// the charge.
    pub fn channel_queue_charge() -> Result<(), String> {
        fresh()?;
        let uid = 9090;
        credentials::set_current(Cred::new(uid, 0, 0, 0, 0));
        quota::set_limit(uid, Resource::QueueDepth, 1);
        let (client, server) = channels::create().map_err(channel_reason)?;
        let first = one_way()?;
        let bytes = first.len() as u64;
        channels::send(client, &first).map_err(channel_reason)?;
        check!(
            quota::usage(uid, Resource::QueueDepth) == 1,
            "the queued message was not charged to its sender's uid"
        );
        check!(
            quota::usage(uid, Resource::QueueBytes) == bytes,
            "queued bytes are {}, expected {bytes}",
            quota::usage(uid, Resource::QueueBytes)
        );
        check!(
            channels::send(client, &one_way()?) == Err(channels::Error::QuotaExceeded),
            "the per-uid queue depth limit was not enforced"
        );
        check!(
            quota::usage(uid, Resource::QueueDepth) == 1,
            "a refused send leaked a queue slot"
        );

        let message = channels::try_recv(server)
            .map_err(channel_reason)?
            .ok_or("the queued message vanished")?;
        check!(!message.bytes.is_empty(), "the delivered message is empty");
        check!(
            quota::usage(uid, Resource::QueueDepth) == 0,
            "delivery did not release the queue slot"
        );
        check!(
            quota::usage(uid, Resource::QueueBytes) == 0,
            "delivery did not release the queued bytes"
        );

        channels::close_endpoint(client).map_err(channel_reason)?;
        channels::close_endpoint(server).map_err(channel_reason)?;
        handles::reset_for_task(task::current());
        credentials::reset_for_task(task::current());
        quota::reset();
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

    /// An exited `clone(CLONE_VM)` thread (parentless) releases its slot once
    /// the scheduler has switched away from it; a task with a parent stays a
    /// waitable zombie until `wait4` reaps it (issue #133).
    pub fn thread_exit_reclaim() -> Result<(), String> {
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        // A process to own the thread: forking from init gives it a table of
        // its own, which the thread then shares.
        let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
        let leader_pml4 = task::harness::pml4(leader).ok_or("the leader has no address space")?;
        task::harness::switch_current(leader);
        let thread = task::spawn_thread("thread", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("thread: {error}"))?;
        check!(
            task::harness::pml4(thread) == Some(leader_pml4),
            "the thread does not share the leader's address space"
        );

        // Exit the thread and run the tick that switches away from it. The
        // slot is only freed after the scheduler flags it, so it must still be
        // present until `reclaim_pending` runs.
        task::harness::finish(thread, 0x33);
        task::harness::switch_current(thread);
        let next = task::harness::simulate_tick();
        check!(next != thread, "the finished thread was selected again");
        check!(
            task::harness::state(thread) == Some(task::TaskState::Done),
            "the thread was reclaimed while still on the scheduler stack"
        );
        task::reclaim_pending();
        check!(
            task::harness::state(thread).is_none(),
            "the exited thread still holds its slot"
        );
        check!(
            task::process::find_by_pid(thread).is_none(),
            "a reclaimed thread is still findable by pid"
        );
        check!(
            task::harness::pml4(leader) == Some(leader_pml4),
            "reclaiming the thread tore down the shared address space"
        );
        check!(
            task::signal::send_tid(
                leader,
                thread,
                task::signal::SIGTERM,
                task::signal::SigInfo::user(leader, 0)
            ) == Err(task::signal::SignalError::NoSuchProcess),
            "a reclaimed thread's tid still accepts signals"
        );

        // A task with a parent is not reclaimed: `wait4` must still collect it.
        task::harness::switch_current(leader);
        let child = task::spawn_fork().map_err(|error| format!("child: {error}"))?;
        task::harness::finish(child, 0x44);
        task::harness::switch_current(child);
        let next = task::harness::simulate_tick();
        check!(next != child, "the finished child was selected again");
        task::reclaim_pending();
        check!(
            task::harness::state(child) == Some(task::TaskState::Done),
            "a waitable child was reclaimed without wait4"
        );
        task::harness::switch_current(leader);
        let (reaped, status) = task::reap_child().ok_or("the child is not reapable")?;
        check!(
            reaped == child && status == 0x44,
            "wait4 collected {reaped}/{status:#x}, expected {child}/0x44"
        );

        // The parentless leader itself is reclaimed once it leaves the CPU.
        task::harness::finish(leader, 0);
        task::harness::switch_current(leader);
        task::harness::simulate_tick();
        task::reclaim_pending();
        check!(
            task::harness::state(leader).is_none(),
            "the exited leader still holds its slot"
        );
        task::harness::reset();
        Ok(())
    }

    /// 64 spawn/exit generations in one process: the freed slot is recycled
    /// every round, past the ~14 raw spawns a 16-slot table allows.
    pub fn thread_churn_generations() -> Result<(), String> {
        const ROUNDS: usize = 64;
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
        let mut slots = Vec::new();
        for round in 0..ROUNDS {
            task::harness::switch_current(leader);
            let thread = task::spawn_thread("thread", process::USER_STACK_TOP, 0, 0)
                .map_err(|error| format!("round {round}: spawn: {error}"))?;
            slots.push(thread);
            task::harness::finish(thread, 0);
            task::harness::switch_current(thread);
            let next = task::harness::simulate_tick();
            check!(
                next != thread,
                "round {round}: finished thread was selected"
            );
            task::reclaim_pending();
            check!(
                task::harness::state(thread).is_none(),
                "round {round}: slot {thread} was not reclaimed"
            );
        }
        check!(
            slots.iter().all(|&slot| slot == slots[0]),
            "the thread slot was not recycled: {slots:?}"
        );
        task::harness::finish(leader, 0);
        task::harness::switch_current(leader);
        task::harness::simulate_tick();
        task::reclaim_pending();
        task::harness::reset();
        Ok(())
    }

    /// Soak (issue #133): thousands of short-lived threads, plus repeated
    /// whole thread-group teardowns with a mapped address space. Occupied slots
    /// and frame accounting must return exactly to the baseline.
    pub fn soak_thread_exit_generations() -> Result<(), String> {
        const THREADS: usize = 4096;
        const GROUPS: usize = 64;
        /// A deliberately generous ceiling (roughly a minute of wall clock);
        /// the loop is expected to take well under a second even under TCG.
        const MAX_CYCLES: u64 = 400_000_000_000;

        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        let baseline_frames = mem::frame_stats().live();
        let baseline_slots = task::process::process_list().len();
        let start = unsafe { core::arch::x86_64::_rdtsc() };

        // One long-lived process whose threads churn: every exit must recycle
        // the slot and drop the task's own buffers.
        let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
        let leader_pml4 =
            PhysAddr::new(task::harness::pml4(leader).ok_or("the leader has no address space")?);
        process::map_range(leader_pml4, TEST_VA, TEST_VA + 4 * 4096)
            .map_err(|error| format!("leader map: {error}"))?;
        let steady_frames = mem::frame_stats().live();
        let mut thread_slots = Vec::new();
        for round in 0..THREADS {
            task::harness::switch_current(leader);
            let thread = task::spawn_thread("thread", process::USER_STACK_TOP, 0, 0)
                .map_err(|error| format!("round {round}: spawn: {error}"))?;
            thread_slots.push(thread);

            // Give the thread task-owned heap state, so reclamation has real
            // buffers to drop, not just an empty shell.
            task::harness::switch_current(thread);
            task::write_output(b"thread output\n");
            task::fd_open(task::Fd::File {
                data: alloc::vec![0x5a; 64],
                offset: 0,
            })
            .ok_or_else(|| format!("round {round}: fd_open failed"))?;
            task::harness::finish(thread, 0);
            let next = task::harness::simulate_tick();
            check!(
                next != thread,
                "round {round}: finished thread was selected"
            );
            task::reclaim_pending();
            check!(
                task::harness::state(thread).is_none(),
                "round {round}: slot {thread} was not reclaimed"
            );
            if round % 1024 == 0 {
                let live = mem::frame_stats().live();
                check!(
                    live == steady_frames,
                    "round {round}: frames leaked ({} over baseline)",
                    live.saturating_sub(steady_frames)
                );
                serial_println!(
                    "TEST:task_soak_thread_exit_generations:PROGRESS:thread {round}/{THREADS}"
                );
            }
        }
        check!(
            thread_slots.iter().all(|&slot| slot == thread_slots[0]),
            "the churn did not recycle one slot"
        );
        check!(
            task::process::process_list().len() == baseline_slots + 1,
            "slots leaked during thread churn: {} rows",
            task::process::process_list().len()
        );

        // Whole generations: each builds its own address space with a mapped
        // page, spawns a thread in it, exits both, and must return the frames.
        for round in 0..GROUPS {
            task::harness::switch_current(task::KERNEL_TASK);
            let group = task::spawn_fork().map_err(|error| format!("group {round}: {error}"))?;
            let group_pml4 =
                PhysAddr::new(task::harness::pml4(group).ok_or("the group has no address space")?);
            process::map_range(group_pml4, TEST_VA, TEST_VA + 4096)
                .map_err(|error| format!("group {round} map: {error}"))?;
            task::harness::switch_current(group);
            let thread = task::spawn_thread("thread", process::USER_STACK_TOP, 0, 0)
                .map_err(|error| format!("group {round}: thread spawn: {error}"))?;
            task::harness::finish(thread, 0);
            task::harness::finish(group, 0);

            // Switch away from the thread: its slot goes, the shared address
            // space stays (the group still references it).
            task::harness::switch_current(thread);
            task::harness::simulate_tick();
            task::reclaim_pending();
            check!(
                task::harness::state(thread).is_none(),
                "group {round}: thread slot was not reclaimed"
            );
            // Switch away from the group: the last user is gone, so the whole
            // address space is torn down.
            task::harness::switch_current(group);
            task::harness::simulate_tick();
            task::reclaim_pending();
            check!(
                task::harness::state(group).is_none(),
                "group {round}: group slot was not reclaimed"
            );
            let live = mem::frame_stats().live();
            check!(
                live == steady_frames,
                "group {round}: address-space frames leaked ({} over baseline)",
                live.saturating_sub(steady_frames)
            );
        }

        // Reclaiming the long-lived leader must return everything to the
        // pre-soak baseline.
        task::harness::finish(leader, 0);
        task::harness::switch_current(leader);
        task::harness::simulate_tick();
        task::reclaim_pending();
        let after_frames = mem::frame_stats().live();
        check!(
            after_frames == baseline_frames,
            "soak leaked {} frames",
            after_frames.saturating_sub(baseline_frames)
        );
        let after_slots = task::process::process_list().len();
        check!(
            after_slots == baseline_slots,
            "soak leaked {} task slots",
            after_slots.saturating_sub(baseline_slots)
        );
        let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
        serial_println!(
            "TEST:task_soak_thread_exit_generations:INFO:threads={THREADS} groups={GROUPS} cycles={cycles}"
        );
        check!(
            cycles < MAX_CYCLES,
            "soak used {cycles} cycles, over the {MAX_CYCLES} budget"
        );
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

    /// `task::introspect::TaskSnapshot` (MCP debug bridge Phase 2,
    /// `docs/mcp-debug-bridge.md`) agrees with `process_list` on every live
    /// slot, and round-trips through its wire encoding byte for byte.
    pub fn task_snapshot_matches_process_list() -> Result<(), String> {
        use crate::task::introspect::TaskSnapshot;

        let chain = fork_chain(2)?;
        task::harness::switch_current(task::KERNEL_TASK);
        let (root, child) = (chain[0], chain[1]);

        let snapshot = TaskSnapshot::snapshot();
        let processes = task::process::process_list();
        check!(
            snapshot.rows.len() == task::MAX_TASKS,
            "snapshot has {} rows, expected MAX_TASKS ({})",
            snapshot.rows.len(),
            task::MAX_TASKS
        );

        for info in &processes {
            let row = snapshot
                .rows
                .get(info.slot)
                .ok_or_else(|| alloc::format!("slot {} missing from snapshot", info.slot))?;
            check!(
                row.live
                    && row.pid as usize == info.pid
                    && row.ppid as usize == info.ppid
                    && row.pgid as usize == info.pgid
                    && row.sid as usize == info.sid
                    && row.name == info.name,
                "snapshot row {row:?} does not match process_list row {info:?}"
            );
        }
        let live_slots = processes.len();
        let live_rows = snapshot.rows.iter().filter(|row| row.live).count();
        check!(
            live_rows == live_slots,
            "snapshot has {live_rows} live rows, process_list has {live_slots}"
        );

        // The root (a fresh fork) and its child both show up with the parent
        // link intact.
        let root_row = &snapshot.rows[root];
        check!(
            root_row.live && root_row.ppid as usize != root,
            "root row is {root_row:?}"
        );
        let child_row = &snapshot.rows[child];
        check!(
            child_row.live && child_row.ppid as usize == root,
            "child row is {child_row:?}"
        );

        // Wire round trip: encode then decode must reproduce every row.
        let bytes = snapshot.to_bytes();
        check!(
            bytes.len() == TaskSnapshot::SIZE,
            "encoded {} bytes, expected {}",
            bytes.len(),
            TaskSnapshot::SIZE
        );
        let decoded =
            TaskSnapshot::from_bytes(&bytes).ok_or("from_bytes rejected a valid block")?;
        check!(
            decoded.rows == snapshot.rows && decoded.version == snapshot.version,
            "decoded snapshot does not match the original"
        );

        finish_and_reap_all(&chain)
    }

    /// Soak: repeatedly fork/reap and snapshot the task table many times,
    /// checking the snapshot is always internally consistent (live count
    /// matches `process_list`, every live row round-trips) and that nothing
    /// leaks a stale row once a task is reaped.
    pub fn task_snapshot_soak_fork_churn() -> Result<(), String> {
        use crate::task::introspect::TaskSnapshot;

        const ITERATIONS: usize = 500;
        for iteration in 0..ITERATIONS {
            let chain = fork_chain(2)?;
            task::harness::switch_current(task::KERNEL_TASK);

            let snapshot = TaskSnapshot::snapshot();
            let processes = task::process::process_list();
            let live_rows = snapshot.rows.iter().filter(|row| row.live).count();
            check!(
                live_rows == processes.len(),
                "iteration {iteration}: {live_rows} live rows, process_list has {}",
                processes.len()
            );
            let bytes = snapshot.to_bytes();
            let decoded = TaskSnapshot::from_bytes(&bytes).ok_or_else(|| {
                alloc::format!("iteration {iteration}: from_bytes rejected a valid block")
            })?;
            check!(
                decoded.rows == snapshot.rows,
                "iteration {iteration}: decoded snapshot does not match the original"
            );

            finish_and_reap_all(&chain)?;
        }

        // After the last reap, every forked slot is gone: only init remains.
        let processes = task::process::process_list();
        check!(
            processes.len() == 1 && processes[0].pid == 0,
            "leaked task rows after {ITERATIONS} fork/reap cycles: {processes:?}"
        );
        serial_println!("TEST:task_snapshot_soak_fork_churn:INFO:iterations={ITERATIONS}");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Pipes, pipe2 and socketpair (issue #135)
// ---------------------------------------------------------------------------

mod pipe_suite {
    use super::*;
    use crate::ipc::pipe::{self, End};

    const O_NONBLOCK: u64 = 0o4000;
    const O_CLOEXEC: u64 = 0o2000000;
    const SOCK_STREAM: u64 = 1;
    const SOCK_CLOEXEC: u64 = 0o2000000;
    const F_GETFD: u64 = 1;
    const F_GETFL: u64 = 3;
    const F_SETFL: u64 = 4;
    const F_DUPFD_CLOEXEC: u64 = 1030;
    const EAGAIN: u64 = (-11i64) as u64;

    /// Register the kernel task and close any descriptor an earlier test left
    /// behind, so pipe-object accounting starts from a clean slate.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        for fd in 3..task::FD_COUNT {
            let _ = task::fd_close(fd);
        }
        check!(
            pipe::Pipe::live() == 0,
            "{} pipes leaked into this test",
            pipe::Pipe::live()
        );
        Ok(())
    }

    /// Whether every descriptor of the current task from 3 up is closed.
    fn fds_clean() -> bool {
        (3..task::FD_COUNT).all(|fd| task::fd_kind(fd) == task::FdKind::Closed)
    }

    fn io_err(error: pipe::Error) -> String {
        format!("pipe I/O: {error:?}")
    }

    /// `pipe`, `pipe2` and `socketpair` through the real syscall dispatch:
    /// creation flags (`O_CLOEXEC`, `O_NONBLOCK`), `F_GETFL`/`F_SETFL`,
    /// `F_GETFD`, data flow, EOF and close.
    pub fn syscalls_create_and_io() -> Result<(), String> {
        fresh()?;

        // pipe(fds): no flags, blocking ends.
        let mut fds = [0i32; 2];
        let ret = process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0);
        check!(ret == 0, "pipe returned {ret:#x}");
        let (r, w) = (fds[0] as usize, fds[1] as usize);
        check!(
            task::fd_kind(r) == task::FdKind::Pipe && task::fd_kind(w) == task::FdKind::Pipe,
            "pipe fds {r}/{w} have wrong kinds"
        );
        check!(
            process::linux::dispatch_for_test(72, r as u64, F_GETFL, 0) == 0,
            "F_GETFL on a plain read end is not O_RDONLY"
        );
        let msg = b"ping";
        let n =
            process::linux::dispatch_for_test(1, w as u64, msg.as_ptr() as u64, msg.len() as u64);
        check!(n == msg.len() as u64, "pipe write returned {n:#x}");
        let mut buf = [0u8; 16];
        let n = process::linux::dispatch_for_test(
            0,
            r as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        );
        check!(n == 4 && &buf[..4] == b"ping", "pipe read returned {n}");
        check!(task::fd_close(w), "closing the write end failed");
        let n = process::linux::dispatch_for_test(
            0,
            r as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        );
        check!(
            n == 0,
            "read after last writer close returned {n:#x}, expected EOF"
        );

        // pipe2(fds, O_CLOEXEC | O_NONBLOCK).
        let ret = process::linux::dispatch_for_test(
            293,
            fds.as_mut_ptr() as u64,
            O_CLOEXEC | O_NONBLOCK,
            0,
        );
        check!(ret == 0, "pipe2 returned {ret:#x}");
        let (r2, w2) = (fds[0] as usize, fds[1] as usize);
        check!(
            task::fd_cloexec(r2) && task::fd_cloexec(w2),
            "pipe2 ignored O_CLOEXEC"
        );
        check!(
            process::linux::dispatch_for_test(72, r2 as u64, F_GETFL, 0) == O_NONBLOCK,
            "F_GETFL does not report O_NONBLOCK"
        );
        check!(
            process::linux::dispatch_for_test(72, r2 as u64, F_GETFD, 0) == 1,
            "F_GETFD does not report FD_CLOEXEC"
        );
        let got = process::linux::dispatch_for_test(
            0,
            r2 as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        );
        check!(got == EAGAIN, "empty O_NONBLOCK read returned {got:#x}");
        check!(
            process::linux::dispatch_for_test(72, r2 as u64, F_SETFL, 0) == 0,
            "F_SETFL(0) failed"
        );
        check!(
            task::fd_status(r2) == Some(0),
            "F_SETFL(0) did not clear O_NONBLOCK: {:?}",
            task::fd_status(r2)
        );
        // F_DUPFD_CLOEXEC shares the pipe end and sets FD_CLOEXEC on the copy.
        let dup = process::linux::dispatch_for_test(72, r2 as u64, F_DUPFD_CLOEXEC, 7);
        let dup = dup as usize;
        check!(
            dup >= 7 && task::fd_kind(dup) == task::FdKind::Pipe && task::fd_cloexec(dup),
            "F_DUPFD_CLOEXEC returned {dup}"
        );
        check!(
            task::fd_close(dup),
            "closing the F_DUPFD_CLOEXEC copy failed"
        );

        // socketpair: AF_UNIX + SOCK_STREAM, data crosses both ways.
        let mut sv = [0i32; 2];
        let ret = process::linux::dispatch_args_for_test(
            53,
            1,
            SOCK_STREAM | SOCK_CLOEXEC,
            0,
            sv.as_mut_ptr() as u64,
        );
        check!(ret == 0, "socketpair returned {ret:#x}");
        let (a, b) = (sv[0] as usize, sv[1] as usize);
        check!(
            task::fd_kind(a) == task::FdKind::Socket && task::fd_kind(b) == task::FdKind::Socket,
            "socketpair fds {a}/{b} have wrong kinds"
        );
        check!(
            task::fd_cloexec(a) && task::fd_cloexec(b),
            "SOCK_CLOEXEC ignored"
        );
        let n =
            process::linux::dispatch_for_test(1, a as u64, msg.as_ptr() as u64, msg.len() as u64);
        check!(n == 4, "socketpair write returned {n:#x}");
        let n = process::linux::dispatch_for_test(
            0,
            b as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        );
        check!(
            n == 4 && &buf[..4] == b"ping",
            "socketpair read returned {n}"
        );
        // musl implements send/recv with sendto/recvfrom: std's capture path
        // reads the socket with `recvfrom`, so both must work (and pipes must
        // answer -ENOTSOCK).
        let n =
            process::linux::dispatch_for_test(44, a as u64, msg.as_ptr() as u64, msg.len() as u64);
        check!(n == 4, "sendto returned {n:#x}");
        let n = process::linux::dispatch_for_test(
            45,
            b as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        );
        check!(n == 4 && &buf[..4] == b"ping", "recvfrom returned {n}");
        let enotsock = (-88i64) as u64;
        let n = process::linux::dispatch_for_test(
            45,
            r as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        );
        check!(n == enotsock, "recvfrom on a pipe returned {n:#x}");

        check!(task::fd_close(a), "closing socket side A failed");
        let n = process::linux::dispatch_for_test(
            0,
            b as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        );
        check!(n == 0, "socketpair read after peer close returned {n:#x}");

        // Cleanup: closing every fd drops the live-pipe count back to zero.
        for fd in [r, r2, w2, b] {
            check!(task::fd_close(fd), "cleanup close of {fd} failed");
        }
        check!(fds_clean(), "a descriptor was left open");
        check!(
            pipe::Pipe::live() == 0,
            "{} pipes survived the test",
            pipe::Pipe::live()
        );
        Ok(())
    }

    /// A pipe whose tail wraps the ring returns exactly the bytes written, in
    /// order, across the wrap.
    pub fn ring_wrap_roundtrip() -> Result<(), String> {
        fresh()?;
        let pipe = pipe::Pipe::new().ok_or("Pipe::new failed")?;
        pipe.acquire(End::Read);
        pipe.acquire(End::Write);
        let cap = pipe::CAPACITY;

        let first: Vec<u8> = (0..cap - 4).map(|i| (i % 251) as u8).collect();
        let n = pipe.write(&first, End::Write, true).map_err(io_err)?;
        check!(
            n == first.len(),
            "first write accepted {n} of {}",
            first.len()
        );
        let mut head = [0u8; 16];
        let n = pipe.read(End::Read, &mut head, true).map_err(io_err)?;
        check!(n == 16 && head == first[..16], "head read is wrong");

        // This write lands where the head used to be, so the tail wraps. The
        // ring has exactly 20 bytes free, so the non-blocking write is partial.
        let tail: Vec<u8> = (0..20u8).map(|i| 0xA0u8.wrapping_add(i)).collect();
        let n = pipe.write(&tail, End::Write, true).map_err(io_err)?;
        check!(n == tail.len(), "wrap write accepted {n}");

        let mut out = vec![0u8; first.len() - 16 + tail.len()];
        let n = pipe.read(End::Read, &mut out, true).map_err(io_err)?;
        check!(n == out.len(), "tail read returned {n} of {}", out.len());
        check!(
            &out[..first.len() - 16] == &first[16..],
            "wrapped data mismatch"
        );
        check!(&out[first.len() - 16..] == &tail[..], "tail data mismatch");

        pipe.release(End::Read);
        pipe.release(End::Write);
        drop(pipe);
        check!(pipe::Pipe::live() == 0, "the wrapped pipe was not freed");
        Ok(())
    }

    /// Parking on the pipe queues and triggering the event wakes the task
    /// synchronously with `Woken` (the mechanism blocking I/O is built on).
    pub fn blocking_read_write_wake() -> Result<(), String> {
        fresh()?;
        let pipe = pipe::Pipe::new().ok_or("Pipe::new failed")?;
        pipe.acquire(End::Read);
        pipe.acquire(End::Write);
        let me = task::current();

        // A reader parked on an empty pipe is woken by a write.
        pipe.park_reader(me);
        check!(
            matches!(
                task::harness::state(me),
                Some(task::TaskState::Blocked { .. })
            ),
            "park_reader did not block the current task"
        );
        let n = pipe.write(b"x", End::Write, true).map_err(io_err)?;
        check!(n == 1, "write accepted {n}");
        check!(
            task::harness::state(me) == Some(task::TaskState::Runnable),
            "a write did not wake the parked reader: {:?}",
            task::harness::state(me)
        );
        check!(
            task::harness::take_wake_reason(me) == Some(task::WakeReason::Woken),
            "reader wake reason is not Woken"
        );
        let mut one = [0u8; 1];
        check!(
            pipe.read(End::Read, &mut one, true).map_err(io_err)? == 1 && one[0] == b'x',
            "the woken reader did not get the written byte"
        );

        // A writer parked on a full pipe is woken by a read.
        let fill = vec![7u8; pipe::CAPACITY];
        let n = pipe.write(&fill, End::Write, true).map_err(io_err)?;
        check!(n == fill.len(), "fill write accepted {n}");
        check!(
            pipe.write(b"y", End::Write, true) == Err(pipe::Error::WouldBlock),
            "a full non-blocking pipe accepted more bytes"
        );
        pipe.park_writer(me);
        check!(
            matches!(
                task::harness::state(me),
                Some(task::TaskState::Blocked { .. })
            ),
            "park_writer did not block the current task"
        );
        let mut byte = [0u8; 1];
        check!(
            pipe.read(End::Read, &mut byte, true).map_err(io_err)? == 1,
            "the drain read did not return a byte"
        );
        check!(
            task::harness::state(me) == Some(task::TaskState::Runnable),
            "a read did not wake the parked writer: {:?}",
            task::harness::state(me)
        );
        check!(
            task::harness::take_wake_reason(me) == Some(task::WakeReason::Woken),
            "writer wake reason is not Woken"
        );
        check!(
            pipe.write(b"y", End::Write, true).map_err(io_err)? == 1,
            "space freed by the read did not accept a byte"
        );

        pipe.release(End::Read);
        pipe.release(End::Write);
        drop(pipe);
        check!(pipe::Pipe::live() == 0, "the pipe was not freed");
        Ok(())
    }

    /// Empty reads are EOF after the last writer closes; writes to a pipe with
    /// no readers are `BrokenPipe` (no SIGPIPE; see `ipc::pipe` docs); a
    /// non-blocking end reports `WouldBlock` instead of parking.
    pub fn eof_epipe_nonblock() -> Result<(), String> {
        fresh()?;
        let pipe = pipe::Pipe::new().ok_or("Pipe::new failed")?;
        pipe.acquire(End::Read);
        pipe.acquire(End::Write);
        let mut buf = [0u8; 8];

        check!(
            pipe.read(End::Read, &mut buf, true) == Err(pipe::Error::WouldBlock),
            "an empty non-blocking read did not report WouldBlock"
        );
        let fill = vec![0x5Au8; pipe::CAPACITY];
        check!(
            pipe.write(&fill, End::Write, true).map_err(io_err)? == pipe::CAPACITY,
            "the pipe did not accept a full ring"
        );
        check!(
            pipe.write(&fill, End::Write, true) == Err(pipe::Error::WouldBlock),
            "a full non-blocking write did not report WouldBlock"
        );
        let mut full = vec![0u8; pipe::CAPACITY];
        let drained = pipe.read(End::Read, &mut full, true).map_err(io_err)?;
        check!(
            drained == pipe::CAPACITY,
            "drained {drained} of {}",
            pipe::CAPACITY
        );

        // Last writer closes: reads return 0 (EOF), poll reports POLLHUP.
        pipe.release(End::Write);
        check!(
            pipe.read(End::Read, &mut buf, true).map_err(io_err)? == 0,
            "read after last writer close is not EOF"
        );
        check!(
            pipe.poll(End::Read, pipe::POLLIN) & pipe::POLLHUP != 0,
            "poll on an EOF read end did not report POLLHUP"
        );

        // Last reader closes: writes fail with -EPIPE, poll reports POLLERR.
        pipe.release(End::Read);
        check!(
            pipe.write(&fill, End::Write, true) == Err(pipe::Error::BrokenPipe),
            "write with no readers did not report BrokenPipe"
        );
        check!(
            pipe.poll(End::Write, pipe::POLLOUT) & pipe::POLLERR != 0,
            "poll on a readerless write end did not report POLLERR"
        );

        drop(pipe);
        check!(pipe::Pipe::live() == 0, "the pipe was not freed");
        Ok(())
    }

    /// `dup` shares the pipe's open file description, `fork` inherits the ends,
    /// and `execve`'s `FD_CLOEXEC` sweep closes only the marked descriptors.
    pub fn dup_fork_cloexec() -> Result<(), String> {
        fresh()?;

        // dup2 clears FD_CLOEXEC on the new descriptor; the exec sweep then
        // closes the originals and keeps the copy.
        let mut fds = [0i32; 2];
        let ret = process::linux::dispatch_for_test(293, fds.as_mut_ptr() as u64, O_CLOEXEC, 0);
        check!(ret == 0, "pipe2 returned {ret:#x}");
        let (r, w) = (fds[0] as usize, fds[1] as usize);
        let ret = process::linux::dispatch_for_test(33, r as u64, 9, 0);
        check!(ret == 9, "dup2 returned {ret:#x}");
        check!(
            !task::fd_cloexec(9) && task::fd_kind(9) == task::FdKind::Pipe,
            "dup2 did not clear FD_CLOEXEC on fd 9"
        );
        let closed = process::linux::close_cloexec_fds();
        check!(
            closed == 2,
            "the exec sweep closed {closed} descriptors, expected 2"
        );
        check!(
            task::fd_kind(r) == task::FdKind::Closed && task::fd_kind(w) == task::FdKind::Closed,
            "the exec sweep kept an FD_CLOEXEC end"
        );
        check!(
            task::fd_kind(9) == task::FdKind::Pipe,
            "the exec sweep closed fd 9"
        );
        // fd 9 is the only reader left and no writers remain: EOF.
        let mut buf = [0u8; 4];
        let n = process::linux::dispatch_for_test(0, 9, buf.as_mut_ptr() as u64, buf.len() as u64);
        check!(n == 0, "read on the duped, writerless end returned {n:#x}");
        check!(task::fd_close(9), "closing fd 9 failed");

        // fork inherits both ends; the child can read what the parent wrote.
        let mut fds = [0i32; 2];
        let ret = process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0);
        check!(ret == 0, "pipe returned {ret:#x}");
        let (r, w) = (fds[0] as usize, fds[1] as usize);
        let child = task::spawn_fork().map_err(to_string)?;
        check!(
            task::harness::fd_kind_at(child, r) == task::FdKind::Pipe
                && task::harness::fd_kind_at(child, w) == task::FdKind::Pipe,
            "the forked child did not inherit the pipe ends"
        );
        let n = task::fd_stream_write(w, b"kid").map_err(io_err)?;
        check!(n == 3, "parent write returned {n}");
        task::harness::switch_current(child);
        let mut got = [0u8; 4];
        let n = task::fd_stream_read(r, &mut got).map_err(io_err)?;
        check!(n == 3 && &got[..3] == b"kid", "child read returned {n}");
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            task::fd_close(r) && task::fd_close(w),
            "parent cleanup failed"
        );
        // Dropping the child's task closes its copies; the pipe is freed.
        task::harness::reset();
        check!(
            pipe::Pipe::live() == 0,
            "{} pipes survived the fork test",
            pipe::Pipe::live()
        );
        check!(fds_clean(), "a descriptor was left open");
        Ok(())
    }

    /// `clone` with `CLONE_VM` but without `CLONE_THREAD` (musl's posix_spawn
    /// vfork child) creates a child that inherits a *copy* of the descriptor
    /// table, is parented to the caller so `wait4`/reaping works, and does not
    /// share the caller's address space (so its `exit_group` fallback cannot
    /// kill the parent).
    pub fn vfork_clone_child() -> Result<(), String> {
        fresh()?;
        let mut fds = [0i32; 2];
        let ret = process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0);
        check!(ret == 0, "pipe returned {ret:#x}");
        let (r, w) = (fds[0] as usize, fds[1] as usize);

        // CLONE_VM | CLONE_VFORK | SIGCHLD, as musl's posix_spawn passes.
        const CLONE_VM: u64 = 0x0000_0100;
        const CLONE_VFORK: u64 = 0x0000_4000;
        const SIGCHLD: u64 = 17;
        let child = process::linux::dispatch_args_for_test(
            56,
            CLONE_VM | CLONE_VFORK | SIGCHLD,
            0x1fff_0000,
            0,
            0,
        );
        let child = child as usize;
        check!(
            (1..task::MAX_TASKS).contains(&child),
            "vfork clone returned {child:#x}"
        );
        check!(
            task::harness::fd_kind_at(child, r) == task::FdKind::Pipe
                && task::harness::fd_kind_at(child, w) == task::FdKind::Pipe,
            "the vfork child did not inherit the descriptor table"
        );
        check!(
            task::harness::state(child) == Some(task::TaskState::Runnable),
            "the vfork child is not runnable"
        );

        // The parent owns it: finish and reap like a fork child.
        task::harness::finish(child, 0x21);
        let (slot, status) = task::reap_child().ok_or("the vfork child is not reapable")?;
        check!(
            slot == child && status == 0x21,
            "reaped slot {slot} status {status:#x}"
        );
        check!(task::fd_close(r) && task::fd_close(w), "cleanup failed");
        check!(
            pipe::Pipe::live() == 0,
            "{} pipes survived the vfork test",
            pipe::Pipe::live()
        );
        Ok(())
    }

    /// Soak: 4 MiB through one pipe, then thousands of pipe create/destroy
    /// cycles. Asserts exact data, no pipe/fd leaks and a stable descriptor
    /// table (a leak would trip the live-pipe cap or `fds_clean`).
    pub fn soak_throughput_and_lifecycle() -> Result<(), String> {
        fresh()?;
        const TOTAL: usize = 4 * 1024 * 1024;
        const CHUNK: usize = 4096;
        let pipe = pipe::Pipe::new().ok_or("Pipe::new failed")?;
        pipe.acquire(End::Read);
        pipe.acquire(End::Write);

        let payload: Vec<u8> = (0..CHUNK).map(|i| (i % 251) as u8).collect();
        let mut out = [0u8; CHUNK];
        let (mut written, mut read) = (0usize, 0usize);
        while read < TOTAL {
            if written < TOTAL {
                match pipe.write(&payload, End::Write, true) {
                    Ok(n) => written += n,
                    Err(pipe::Error::WouldBlock) => {}
                    Err(error) => return Err(io_err(error)),
                }
            }
            match pipe.read(End::Read, &mut out, true) {
                Ok(0) => return Err(String::from("soak pipe hit unexpected EOF")),
                Ok(n) => {
                    for (i, &byte) in out[..n].iter().enumerate() {
                        // The stream is the payload repeated, so the expected
                        // byte at stream offset `k` is `(k % CHUNK) % 251`.
                        let expected = (((read + i) % CHUNK) % 251) as u8;
                        check!(
                            byte == expected,
                            "soak mismatch at {read}+{i}: {byte} != {expected}"
                        );
                    }
                    read += n;
                }
                Err(pipe::Error::WouldBlock) => {}
                Err(error) => return Err(io_err(error)),
            }
        }
        check!(written == TOTAL, "pipe accepted {written} of {TOTAL} bytes");
        pipe.release(End::Read);
        pipe.release(End::Write);
        drop(pipe);
        check!(pipe::Pipe::live() == 0, "the soaked pipe was not freed");

        // Create/destroy churn through the descriptor table.
        for round in 0..2000 {
            let pipe =
                pipe::Pipe::new().ok_or_else(|| format!("round {round}: Pipe::new failed"))?;
            let r = task::fd_open(task::Fd::pipe_end(
                alloc::sync::Arc::clone(&pipe),
                End::Read,
            ))
            .ok_or_else(|| format!("round {round}: fd_open(read) failed"))?;
            let w = task::fd_open(task::Fd::pipe_end(
                alloc::sync::Arc::clone(&pipe),
                End::Write,
            ))
            .ok_or_else(|| format!("round {round}: fd_open(write) failed"))?;
            check!(task::fd_close(r), "round {round}: close(read) failed");
            check!(task::fd_close(w), "round {round}: close(write) failed");
        }
        check!(
            pipe::Pipe::live() == 0,
            "{} pipes leaked after the create/destroy churn",
            pipe::Pipe::live()
        );
        check!(fds_clean(), "the churn leaked a descriptor");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Linux ABI round 2: mremap, epoll/eventfd, seqpacket, unix sockets
// ---------------------------------------------------------------------------

mod linux_suite {
    use super::*;
    use crate::ipc::pipe;
    use crate::ipc::unix;

    const PAGE: u64 = 4096;
    const PROT_RW: u64 = 3;
    const MAP_PRIVATE: u64 = 0x02;
    const MAP_FIXED: u64 = 0x10;
    const MAP_ANONYMOUS: u64 = 0x20;
    const MREMAP_MAYMOVE: u64 = 1;
    const MREMAP_FIXED: u64 = 2;

    const AF_UNIX: u64 = 1;
    const SOCK_STREAM: u64 = 1;
    const SOCK_SEQPACKET: u64 = 5;
    const SOCK_CLOEXEC: u64 = 0o2000000;

    const EPOLL_CTL_ADD: u64 = 1;
    const EPOLL_CTL_DEL: u64 = 2;
    const EPOLL_CTL_MOD: u64 = 3;
    const EPOLLIN: u32 = 0x0001;
    const EPOLLHUP: u32 = 0x0010;
    const EPOLLET: u32 = 0x8000_0000;

    const O_NONBLOCK: u64 = 0o4000;
    const EFD_SEMAPHORE: u64 = 1;

    const EAGAIN: u64 = (-11i64) as u64;
    const EEXIST: u64 = (-17i64) as u64;
    const ENOENT: u64 = (-2i64) as u64;
    const EINVAL: u64 = (-22i64) as u64;
    const EMSGSIZE: u64 = (-90i64) as u64;

    /// Register the kernel task with a bump region, close leftover
    /// descriptors, and forget any bound socket names, so each test starts
    /// from a clean ABI surface.
    fn fresh() -> Result<(), String> {
        task::register_kernel();
        for fd in 3..task::FD_COUNT {
            let _ = task::fd_close(fd);
        }
        unix::clear_for_test();
        let table = crate::mem::kernel_table();
        task::register_bumps(
            table.as_u64(),
            process::linux::BRK_BASE,
            process::linux::MMAP_BASE,
        );
        Ok(())
    }

    fn fds_clean() -> bool {
        (3..task::FD_COUNT).all(|fd| task::fd_kind(fd) == task::FdKind::Closed)
    }

    fn mmap_fixed(base: u64, len: u64) -> u64 {
        process::linux::dispatch_args5_for_test(
            9,
            base,
            len,
            PROT_RW,
            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
            0,
        )
    }

    fn mremap(old: u64, old_size: u64, new_size: u64, flags: u64, new_addr: u64) -> u64 {
        process::linux::dispatch_args5_for_test(25, old, old_size, new_size, flags, new_addr)
    }

    fn munmap(base: u64, len: u64) -> u64 {
        process::linux::dispatch_for_test(11, base, len, 0)
    }

    /// Deterministic fill pattern, mirroring the pipe soak's.
    fn fill(addr: u64, seed: u8, len: usize) {
        for offset in 0..len {
            let byte = seed ^ (offset as u8).wrapping_mul(31);
            // Safety: the test maps this page range into the kernel's user half.
            unsafe { (addr as *mut u8).add(offset).write_volatile(byte) };
        }
    }

    fn matches(addr: u64, seed: u8, len: usize) -> bool {
        (0..len).all(|offset| {
            let byte = seed ^ (offset as u8).wrapping_mul(31);
            // Safety: same mapped range as `fill`.
            unsafe { (addr as *const u8).add(offset).read_volatile() == byte }
        })
    }

    fn epoll_event(events: u32, data: u64) -> [u8; 12] {
        let mut buf = [0u8; 12];
        buf[..4].copy_from_slice(&events.to_le_bytes());
        buf[4..].copy_from_slice(&data.to_le_bytes());
        buf
    }

    /// Unpack one packed `struct epoll_event` from a ready array.
    fn unpack_event(out: &[u8]) -> (u32, u64) {
        let events = u32::from_le_bytes(out[..4].try_into().unwrap());
        let data = u64::from_le_bytes(out[4..12].try_into().unwrap());
        (events, data)
    }

    fn epoll_ctl(epfd: u64, op: u64, fd: u64, event: &[u8; 12]) -> u64 {
        process::linux::dispatch_args_for_test(233, epfd, op, fd, event.as_ptr() as u64)
    }

    fn epoll_wait0(epfd: u64, out: &mut [u8]) -> u64 {
        let max = (out.len() / 12) as u64;
        process::linux::dispatch_args_for_test(232, epfd, out.as_ptr() as u64, max, 0)
    }

    fn read_fd(fd: u64, buf: &mut [u8]) -> u64 {
        process::linux::dispatch_for_test(0, fd, buf.as_mut_ptr() as u64, buf.len() as u64)
    }

    fn write_fd(fd: u64, buf: &[u8]) -> u64 {
        process::linux::dispatch_for_test(1, fd, buf.as_ptr() as u64, buf.len() as u64)
    }

    fn socketpair(kind: u64) -> Result<(u64, u64), String> {
        let mut sv = [0i32; 2];
        let ret =
            process::linux::dispatch_args_for_test(53, AF_UNIX, kind, 0, sv.as_mut_ptr() as u64);
        check!(ret == 0, "socketpair({kind:#x}) returned {ret:#x}");
        Ok((sv[0] as u64, sv[1] as u64))
    }

    /// `mremap` grows and shrinks in place, then relocates with `MAYMOVE` and
    /// `MREMAP_FIXED`, keeping the pages' contents throughout.
    pub fn mremap_grow_shrink_move() -> Result<(), String> {
        fresh()?;
        let table = crate::mem::kernel_table();
        let (vsz, frames) = crate::mem::vma_stats(table);
        let base = process::linux::MMAP_BASE;
        check!(
            mmap_fixed(base, 2 * PAGE) == base,
            "mmap did not land at {base:#x}"
        );
        fill(base, 0x11, 2 * PAGE as usize);

        // Grow 2 -> 4 pages in place: the free range above is claimed.
        check!(
            mremap(base, 2 * PAGE, 4 * PAGE, 0, 0) == base,
            "in-place grow moved the mapping"
        );
        check!(matches(base, 0x11, 2 * PAGE as usize), "grow lost data");
        // Safety: the grown page is mapped into the kernel's user half.
        check!(
            unsafe { (base as *const u8).add(2 * PAGE as usize).read_volatile() } == 0,
            "grown page is not demand-zero"
        );

        // Shrink 4 -> 1 page: the tail is gone, the head intact.
        check!(
            mremap(base, 4 * PAGE, PAGE, 0, 0) == base,
            "in-place shrink moved the mapping"
        );
        check!(matches(base, 0x11, PAGE as usize), "shrink lost data");
        check!(
            crate::mem::vma::find(table, base + PAGE).is_none(),
            "shrunk tail still has a VMA"
        );

        // Relocate 1 -> 2 pages with MAYMOVE; a blocker mapping above forces the
        // move instead of an in-place grow.
        check!(
            mmap_fixed(base + PAGE, PAGE) == base + PAGE,
            "blocker mmap failed"
        );
        let moved = mremap(base, PAGE, 2 * PAGE, MREMAP_MAYMOVE, 0);
        check!(moved != 0 && (moved as i64) > 0, "move returned {moved:#x}");
        check!(moved != base, "move kept the old address");
        check!(matches(moved, 0x11, PAGE as usize), "move lost data");
        check!(
            crate::mem::vma::find(table, base).is_none(),
            "old VMA survived the move"
        );

        // MREMAP_FIXED places the range exactly.
        let dest = process::linux::MMAP_BASE + 0x40_0000;
        check!(
            mremap(
                moved,
                2 * PAGE,
                2 * PAGE,
                MREMAP_MAYMOVE | MREMAP_FIXED,
                dest
            ) == dest,
            "fixed move did not land at {dest:#x}"
        );
        check!(matches(dest, 0x11, PAGE as usize), "fixed move lost data");

        check!(munmap(dest, 2 * PAGE) == 0, "cleanup munmap failed");
        check!(munmap(base + PAGE, PAGE) == 0, "blocker munmap failed");
        let (vsz_after, frames_after) = crate::mem::vma_stats(table);
        check!(
            vsz_after == vsz,
            "mremap leaked VMA bytes: {vsz_after} != {vsz}"
        );
        check!(
            frames_after == frames,
            "mremap leaked frames: {frames_after} != {frames}"
        );
        Ok(())
    }

    /// Soak: repeated map/grow/relocate/shrink/unmap generations must not leak
    /// VMAs, frames or quota.
    pub fn mremap_soak_churn() -> Result<(), String> {
        fresh()?;
        let table = crate::mem::kernel_table();
        let (vsz, frames) = crate::mem::vma_stats(table);
        for round in 0..1000u32 {
            let base = process::linux::MMAP_BASE + 0x100_0000 + (round as u64 % 8) * 0x1_0000;
            check!(
                mmap_fixed(base, 2 * PAGE) == base,
                "round {round}: mmap failed"
            );
            fill(base, round as u8, 2 * PAGE as usize);
            let grown = mremap(base, 2 * PAGE, 3 * PAGE, MREMAP_MAYMOVE, 0);
            check!(
                grown != 0 && (grown as i64) > 0,
                "round {round}: grow returned {grown:#x}"
            );
            check!(
                matches(grown, round as u8, PAGE as usize),
                "round {round}: relocated page lost data"
            );
            // Touch the newly grown page so a frame is actually resident.
            // Safety: within the relocated mapping.
            unsafe {
                (grown as *mut u8)
                    .add(2 * PAGE as usize)
                    .write_volatile(0x5A)
            };
            let shrunk = mremap(grown, 3 * PAGE, PAGE, MREMAP_MAYMOVE, 0);
            check!(
                shrunk != 0 && (shrunk as i64) > 0,
                "round {round}: shrink returned {shrunk:#x}"
            );
            check!(
                matches(shrunk, round as u8, PAGE as usize),
                "round {round}: shrunk page lost data"
            );
            check!(munmap(shrunk, PAGE) == 0, "round {round}: munmap failed");
        }
        let (vsz_after, frames_after) = crate::mem::vma_stats(table);
        check!(
            vsz_after == vsz,
            "soak leaked VMA bytes: {vsz_after} != {vsz}"
        );
        check!(
            frames_after == frames,
            "soak leaked frames: {frames_after} != {frames}"
        );
        Ok(())
    }

    /// `eventfd` read/write semantics: drain-to-zero, non-blocking `EAGAIN`,
    /// `EINVAL` on `u64::MAX`, and `EFD_SEMAPHORE` decrements.
    pub fn eventfd_semantics() -> Result<(), String> {
        fresh()?;
        let efd = process::linux::dispatch_for_test(290, 5, O_NONBLOCK, 0);
        check!((efd as i64) > 0, "eventfd2 returned {efd:#x}");
        let mut value = [0u8; 8];
        check!(read_fd(efd, &mut value) == 8, "eventfd read length");
        check!(u64::from_le_bytes(value) == 5, "eventfd initial value");
        check!(read_fd(efd, &mut value) == EAGAIN, "empty eventfd read");
        let three = 3u64.to_le_bytes();
        check!(write_fd(efd, &three) == 8, "eventfd write");
        check!(
            read_fd(efd, &mut value) == 8 && u64::from_le_bytes(value) == 3,
            "add then drain"
        );
        let max = u64::MAX.to_le_bytes();
        check!(write_fd(efd, &max) == EINVAL, "eventfd u64::MAX write");

        let sem = process::linux::dispatch_for_test(290, 2, EFD_SEMAPHORE | O_NONBLOCK, 0);
        check!((sem as i64) > 0, "semaphore eventfd returned {sem:#x}");
        for expected in [1u64, 1] {
            check!(read_fd(sem, &mut value) == 8, "semaphore read length");
            check!(u64::from_le_bytes(value) == expected, "semaphore value");
        }
        check!(read_fd(sem, &mut value) == EAGAIN, "drained semaphore");

        check!(task::fd_close(efd as usize), "close eventfd failed");
        check!(task::fd_close(sem as usize), "close semaphore failed");
        check!(fds_clean(), "eventfd test left a descriptor");
        Ok(())
    }

    /// `epoll`: level trigger, zero timeout, `EPOLLET` edges, and `EPOLLHUP`.
    pub fn epoll_level_edge_hangup() -> Result<(), String> {
        fresh()?;
        let mut fds = [0i32; 2];
        let ret = process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0);
        check!(ret == 0, "pipe returned {ret:#x}");
        let (r, w) = (fds[0] as u64, fds[1] as u64);
        let epfd = process::linux::dispatch_for_test(291, SOCK_CLOEXEC, 0, 0);
        check!((epfd as i64) > 0, "epoll_create1 returned {epfd:#x}");

        let interest = epoll_event(EPOLLIN, 0x1234);
        check!(
            epoll_ctl(epfd, EPOLL_CTL_ADD, r, &interest) == 0,
            "ADD failed"
        );
        check!(
            epoll_ctl(epfd, EPOLL_CTL_ADD, r, &interest) == EEXIST,
            "duplicate ADD not rejected"
        );
        let mut out = [0u8; 24];
        check!(epoll_wait0(epfd, &mut out) == 0, "idle epoll_wait woke");

        check!(write_fd(w, b"a") == 1, "pipe write");
        check!(
            epoll_wait0(epfd, &mut out) == 1,
            "readable pipe not reported"
        );
        let (events, data) = unpack_event(&out);
        check!(events & EPOLLIN != 0, "missing EPOLLIN: {events:#x}");
        check!(data == 0x1234, "user data lost: {data:#x}");
        // Level trigger: still ready while the byte sits undrained.
        check!(
            epoll_wait0(epfd, &mut out) == 1,
            "level trigger drained early"
        );

        // Edge trigger: one report, then silence until the stream changes.
        let edge = epoll_event(EPOLLIN | EPOLLET, 0x9);
        check!(epoll_ctl(epfd, EPOLL_CTL_MOD, r, &edge) == 0, "MOD failed");
        check!(
            epoll_wait0(epfd, &mut out) == 1,
            "edge did not report on arm"
        );
        check!(
            epoll_wait0(epfd, &mut out) == 0,
            "edge repeated without a change"
        );
        let mut one = [0u8; 1];
        check!(read_fd(r, &mut one) == 1, "drain read");
        check!(write_fd(w, b"b") == 1, "second pipe write");
        check!(
            epoll_wait0(epfd, &mut out) == 1,
            "new data did not re-arm the edge"
        );

        // Hangup: the write end closes, so the interest reports EPOLLHUP.
        check!(task::fd_close(w as usize), "close write end failed");
        let level = epoll_event(EPOLLIN, 0x77);
        check!(
            epoll_ctl(epfd, EPOLL_CTL_MOD, r, &level) == 0,
            "MOD for HUP failed"
        );
        check!(epoll_wait0(epfd, &mut out) == 1, "hangup not reported");
        let (events, data) = unpack_event(&out);
        check!(events & EPOLLHUP != 0, "missing EPOLLHUP: {events:#x}");
        check!(data == 0x77, "hangup data lost");

        check!(epoll_ctl(epfd, EPOLL_CTL_DEL, r, &level) == 0, "DEL failed");
        check!(
            epoll_wait0(epfd, &mut out) == 0,
            "deleted interest still ready"
        );
        check!(
            epoll_ctl(epfd, EPOLL_CTL_DEL, r, &level) == ENOENT,
            "duplicate DEL not rejected"
        );

        check!(task::fd_close(r as usize), "close read end failed");
        check!(task::fd_close(epfd as usize), "close epoll failed");
        check!(fds_clean(), "epoll test left a descriptor");
        check!(pipe::Pipe::live() == 0, "the pipe was not freed");
        Ok(())
    }

    /// `EPOLLET` edges that are ready but beyond `maxevents` stay pending: each
    /// later wait reports the next one instead of silently marking it seen.
    pub fn epoll_edge_over_maxevents() -> Result<(), String> {
        fresh()?;
        let epfd = process::linux::dispatch_for_test(291, 0, 0, 0);
        check!((epfd as i64) > 0, "epoll_create1 returned {epfd:#x}");
        let mut ends: Vec<(u64, u64)> = Vec::new();
        for tag in 1..=3u64 {
            let mut fds = [0i32; 2];
            check!(
                process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0) == 0,
                "pipe {tag} failed"
            );
            let (r, w) = (fds[0] as u64, fds[1] as u64);
            let edge = epoll_event(EPOLLIN | EPOLLET, tag);
            check!(epoll_ctl(epfd, EPOLL_CTL_ADD, r, &edge) == 0, "ADD {tag} failed");
            check!(write_fd(w, b"x") == 1, "pipe {tag} write");
            ends.push((r, w));
        }
        let mut one = [0u8; 12];
        let mut seen: Vec<u64> = Vec::new();
        for round in 0..3 {
            check!(
                epoll_wait0(epfd, &mut one) == 1,
                "round {round}: a pending edge was lost past maxevents"
            );
            let (_, data) = unpack_event(&one);
            check!(!seen.contains(&data), "round {round}: edge {data} reported twice");
            seen.push(data);
        }
        check!(
            epoll_wait0(epfd, &mut one) == 0,
            "an edge repeated after every edge was reported"
        );
        for (r, w) in ends {
            check!(task::fd_close(r as usize), "close read end failed");
            check!(task::fd_close(w as usize), "close write end failed");
        }
        check!(task::fd_close(epfd as usize), "close epoll failed");
        check!(fds_clean(), "epoll edge test left a descriptor");
        check!(pipe::Pipe::live() == 0, "a pipe was not freed");
        Ok(())
    }

    /// With `maxevents = 1`, a level-triggered interest that stays ready must
    /// not starve interests registered after it: successive waits rotate
    /// through the ready set, as Linux does.
    pub fn epoll_level_does_not_starve() -> Result<(), String> {
        fresh()?;
        let epfd = process::linux::dispatch_for_test(291, 0, 0, 0);
        check!((epfd as i64) > 0, "epoll_create1 returned {epfd:#x}");
        let mut ends: Vec<(u64, u64)> = Vec::new();
        for (tag, events) in [(1u64, EPOLLIN), (2, EPOLLIN | EPOLLET), (3, EPOLLIN)] {
            let mut fds = [0i32; 2];
            check!(
                process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0) == 0,
                "pipe {tag} failed"
            );
            let (r, w) = (fds[0] as u64, fds[1] as u64);
            let interest = epoll_event(events, tag);
            check!(epoll_ctl(epfd, EPOLL_CTL_ADD, r, &interest) == 0, "ADD {tag} failed");
            check!(write_fd(w, b"x") == 1, "pipe {tag} write");
            ends.push((r, w));
        }
        let mut one = [0u8; 12];
        let mut seen: Vec<u64> = Vec::new();
        for round in 0..6 {
            check!(
                epoll_wait0(epfd, &mut one) == 1,
                "round {round}: nothing reported"
            );
            let (_, data) = unpack_event(&one);
            if !seen.contains(&data) {
                seen.push(data);
            }
        }
        seen.sort();
        check!(
            seen == [1, 2, 3],
            "maxevents=1 waits starved an interest: saw {seen:?}"
        );
        for (r, w) in ends {
            check!(task::fd_close(r as usize), "close read end failed");
            check!(task::fd_close(w as usize), "close write end failed");
        }
        check!(task::fd_close(epfd as usize), "close epoll failed");
        check!(fds_clean(), "epoll starvation test left a descriptor");
        check!(pipe::Pipe::live() == 0, "a pipe was not freed");
        Ok(())
    }

    /// Soak: thousands of `epoll_ctl` add/mod/del cycles over mixed targets,
    /// and a full interest set, with no descriptor, pipe or interest leak.
    pub fn epoll_soak_add_wait_cycles() -> Result<(), String> {
        fresh()?;
        let epfd = process::linux::dispatch_for_test(291, 0, 0, 0);
        check!((epfd as i64) > 0, "epoll_create1 returned {epfd:#x}");
        let efd = process::linux::dispatch_for_test(290, 0, O_NONBLOCK, 0);
        check!((efd as i64) > 0, "eventfd2 returned {efd:#x}");
        let mut fds = [0i32; 2];
        check!(
            process::linux::dispatch_for_test(22, fds.as_mut_ptr() as u64, 0, 0) == 0,
            "pipe failed"
        );
        let (r, w) = (fds[0] as u64, fds[1] as u64);
        let mut out = [0u8; 12];
        let interest = epoll_event(EPOLLIN, 0);
        for round in 0..20_000u64 {
            let target = if round % 2 == 0 { efd } else { r };
            check!(
                epoll_ctl(epfd, EPOLL_CTL_ADD, target, &interest) == 0,
                "round {round}: ADD failed"
            );
            let _ = epoll_wait0(epfd, &mut out);
            let changed = epoll_event(EPOLLIN, round);
            check!(
                epoll_ctl(epfd, EPOLL_CTL_MOD, target, &changed) == 0,
                "round {round}: MOD failed"
            );
            check!(
                epoll_ctl(epfd, EPOLL_CTL_DEL, target, &interest) == 0,
                "round {round}: DEL failed"
            );
        }

        // A full interest set: eight eventfds, all made ready at once.
        let mut events: Vec<u64> = Vec::new();
        for value in 1..=8u64 {
            let fd = process::linux::dispatch_for_test(290, value, O_NONBLOCK, 0);
            check!((fd as i64) > 0, "eventfd {value} returned {fd:#x}");
            let interest = epoll_event(EPOLLIN, value);
            check!(
                epoll_ctl(epfd, EPOLL_CTL_ADD, fd, &interest) == 0,
                "ADD eventfd {value} failed"
            );
            events.push(fd);
        }
        for fd in &events {
            let one = 1u64.to_le_bytes();
            check!(write_fd(*fd, &one) == 8, "eventfd write failed");
        }
        let mut ready = [0u8; 12 * 8];
        check!(
            epoll_wait0(epfd, &mut ready) == 8,
            "not all interests ready"
        );
        let mut seen = [false; 9];
        for index in 0..8 {
            let (bits, data) = unpack_event(&ready[index * 12..]);
            check!(bits & EPOLLIN != 0, "ready event {index} lacks EPOLLIN");
            seen[data as usize] = true;
        }
        check!(
            (1..=8).all(|value| seen[value]),
            "ready data values lost: {seen:?}"
        );

        // Closing a registered descriptor drops its interest.
        check!(task::fd_close(events[0] as usize), "close eventfd failed");
        check!(
            epoll_ctl(epfd, EPOLL_CTL_DEL, events[0], &interest) == ENOENT,
            "closed descriptor kept its interest"
        );
        for fd in events.iter().skip(1) {
            check!(task::fd_close(*fd as usize), "close eventfd failed");
            let _ = epoll_ctl(epfd, EPOLL_CTL_DEL, *fd, &interest);
        }
        check!(task::fd_close(efd as usize), "close eventfd failed");
        check!(task::fd_close(epfd as usize), "close epoll failed");
        for fd in [r, w] {
            check!(task::fd_close(fd as usize), "close pipe fd failed");
        }
        check!(fds_clean(), "epoll soak leaked a descriptor");
        check!(pipe::Pipe::live() == 0, "epoll soak leaked a pipe");
        Ok(())
    }

    /// `SOCK_SEQPACKET`: one read per message, truncation discards the rest,
    /// `-EMSGSIZE` over capacity, and EOF after the peer closes.
    pub fn seqpacket_boundaries() -> Result<(), String> {
        fresh()?;
        let (a, b) = socketpair(AF_UNIX | SOCK_SEQPACKET)?;
        check!(write_fd(a, b"hello") == 5, "seqpacket write");
        let mut small = [0u8; 3];
        check!(read_fd(b, &mut small) == 3, "truncated read length");
        check!(&small == b"hel", "truncated read contents");
        // The discarded tail must not leak into the next message.
        check!(write_fd(a, b"xy") == 2, "second seqpacket write");
        let mut big = [0u8; 8];
        check!(read_fd(b, &mut big) == 2, "message after truncation");
        check!(&big[..2] == b"xy", "message boundary lost after truncation");
        // Back-to-back messages stay distinct.
        check!(write_fd(a, b"one") == 3, "write one");
        check!(write_fd(a, b"two") == 3, "write two");
        check!(
            read_fd(b, &mut big) == 3 && &big[..3] == b"one",
            "first message"
        );
        check!(
            read_fd(b, &mut big) == 3 && &big[..3] == b"two",
            "second message"
        );
        // Over-capacity messages are refused whole.
        let huge = vec![0u8; pipe::CAPACITY + 1];
        check!(
            write_fd(a, &huge) == EMSGSIZE,
            "oversized seqpacket write was not EMSGSIZE"
        );
        check!(task::fd_close(a as usize), "close A failed");
        check!(read_fd(b, &mut big) == 0, "seqpacket EOF");
        check!(task::fd_close(b as usize), "close B failed");
        check!(fds_clean(), "seqpacket test left a descriptor");
        check!(pipe::Pipe::live() == 0, "seqpacket test leaked a pipe");
        Ok(())
    }

    /// Soak: many seqpacket messages of varying length keep their boundaries.
    pub fn seqpacket_soak_messages() -> Result<(), String> {
        fresh()?;
        let (a, b) = socketpair(AF_UNIX | SOCK_SEQPACKET)?;
        let mut payload = [0u8; 64];
        let mut out = [0u8; 128];
        for round in 0..20_000u32 {
            let len = (round as usize % 64) + 1;
            for (index, byte) in payload[..len].iter_mut().enumerate() {
                *byte = (round as u8) ^ (index as u8);
            }
            let sent = write_fd(a, &payload[..len]);
            check!(sent == len as u64, "round {round}: write returned {sent}");
            let got = read_fd(b, &mut out);
            check!(got == len as u64, "round {round}: read returned {got}");
            check!(
                out[..len] == payload[..len],
                "round {round}: message contents crossed"
            );
        }
        check!(task::fd_close(a as usize), "close A failed");
        check!(task::fd_close(b as usize), "close B failed");
        check!(fds_clean(), "seqpacket soak leaked a descriptor");
        check!(pipe::Pipe::live() == 0, "seqpacket soak leaked a pipe");
        Ok(())
    }

    /// Stream `socketpair`: EOF on close and `shutdown(SHUT_WR)` half-close.
    pub fn unix_pair_eof_shutdown() -> Result<(), String> {
        fresh()?;
        let (a, b) = socketpair(SOCK_STREAM | SOCK_CLOEXEC)?;
        check!(write_fd(a, b"ping") == 4, "pair write");
        let mut buf = [0u8; 8];
        check!(
            read_fd(b, &mut buf) == 4 && &buf[..4] == b"ping",
            "pair read"
        );
        // Half-close: the peer sees EOF, this end still reads.
        check!(
            process::linux::dispatch_for_test(48, a, 1, 0) == 0,
            "shutdown(SHUT_WR) failed"
        );
        check!(read_fd(b, &mut buf) == 0, "shutdown did not EOF the peer");
        check!(write_fd(b, b"pong") == 4, "peer write after half-close");
        check!(
            read_fd(a, &mut buf) == 4 && &buf[..4] == b"pong",
            "half-closed read"
        );
        check!(
            process::linux::dispatch_for_test(48, a, 2, 0) == 0,
            "shutdown(SHUT_RDWR) failed"
        );
        check!(read_fd(a, &mut buf) == 0, "SHUT_RDWR did not EOF us");

        // A separate pair: closing one end reports EOF to the other.
        let (c, d) = socketpair(SOCK_STREAM)?;
        check!(write_fd(c, b"bye") == 3, "close-EOF write");
        check!(task::fd_close(c as usize), "close C failed");
        check!(read_fd(d, &mut buf) == 3, "close-EOF buffered read");
        check!(read_fd(d, &mut buf) == 0, "close-EOF read");

        for fd in [a, b, d] {
            check!(task::fd_close(fd as usize), "cleanup close failed");
        }
        check!(fds_clean(), "unix pair test left a descriptor");
        check!(pipe::Pipe::live() == 0, "unix pair test leaked a pipe");
        Ok(())
    }

    /// Pathname `AF_UNIX`: `socket`/`bind`/`listen`/`connect`/`accept4` with
    /// data exchange and a clean unregister on close.
    pub fn unix_pathname_bind_connect_accept() -> Result<(), String> {
        fresh()?;
        let listener_fd = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
        check!((listener_fd as i64) > 0, "socket returned {listener_fd:#x}");
        let name = b"/tmp/abi-suite.sock";
        let mut sockaddr = [0u8; 110];
        sockaddr[..2].copy_from_slice(&(AF_UNIX as u16).to_le_bytes());
        sockaddr[2..2 + name.len()].copy_from_slice(name);
        let addr_ptr = sockaddr.as_ptr() as u64;
        let addr_len = (2 + name.len()) as u64;
        check!(
            process::linux::dispatch_for_test(49, listener_fd, addr_ptr, addr_len) == 0,
            "bind failed"
        );
        check!(
            process::linux::dispatch_for_test(50, listener_fd, 8, 0) == 0,
            "listen failed"
        );
        // Rebinding the same name is refused.
        let other = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
        let duplicate = process::linux::dispatch_for_test(49, other, addr_ptr, addr_len);
        check!(
            duplicate == (-98i64) as u64,
            "duplicate bind returned {duplicate:#x}"
        );
        check!(task::fd_close(other as usize), "close other failed");

        let client_fd = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
        check!(
            (client_fd as i64) > 0,
            "client socket returned {client_fd:#x}"
        );
        check!(
            process::linux::dispatch_for_test(42, client_fd, addr_ptr, addr_len) == 0,
            "connect failed"
        );
        let server_fd =
            process::linux::dispatch_args_for_test(288, listener_fd, 0, 0, SOCK_CLOEXEC);
        check!((server_fd as i64) > 0, "accept4 returned {server_fd:#x}");
        check!(write_fd(client_fd, b"hello") == 5, "client write");
        let mut buf = [0u8; 8];
        check!(read_fd(server_fd, &mut buf) == 5, "server read");
        check!(&buf[..5] == b"hello", "server data");
        check!(write_fd(server_fd, b"world") == 5, "server write");
        check!(read_fd(client_fd, &mut buf) == 5, "client read");
        check!(&buf[..5] == b"world", "client data");

        for fd in [client_fd, server_fd, listener_fd] {
            check!(task::fd_close(fd as usize), "cleanup close failed");
        }
        check!(fds_clean(), "pathname test left a descriptor");
        check!(unix::bound_count() == 0, "bound name survived its listener");
        check!(pipe::Pipe::live() == 0, "pathname test leaked a pipe");
        Ok(())
    }

    /// As on Linux, a connection is established at `connect`: the client can
    /// write before the server accepts and the data waits in the pair. A
    /// listener closed with a connection still pending releases the server
    /// side, so the client reads EOF and nothing leaks.
    pub fn unix_write_before_accept() -> Result<(), String> {
        fresh()?;
        let name = b"/tmp/abi-early.sock";
        let mut sockaddr = [0u8; 110];
        sockaddr[..2].copy_from_slice(&(AF_UNIX as u16).to_le_bytes());
        sockaddr[2..2 + name.len()].copy_from_slice(name);
        let addr_ptr = sockaddr.as_ptr() as u64;
        let addr_len = (2 + name.len()) as u64;
        let listener = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
        check!(
            process::linux::dispatch_for_test(49, listener, addr_ptr, addr_len) == 0,
            "bind failed"
        );
        check!(
            process::linux::dispatch_for_test(50, listener, 4, 0) == 0,
            "listen failed"
        );

        // 1. Write before accept buffers instead of failing with -EPIPE.
        let client = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
        check!(
            process::linux::dispatch_for_test(42, client, addr_ptr, addr_len) == 0,
            "connect failed"
        );
        let early = write_fd(client, b"early");
        check!(early == 5, "write before accept returned {early:#x}");
        let server = process::linux::dispatch_args_for_test(288, listener, 0, 0, 0);
        check!((server as i64) > 0, "accept4 returned {server:#x}");
        let mut buf = [0u8; 8];
        check!(read_fd(server, &mut buf) == 5, "server read");
        check!(&buf[..5] == b"early", "early data lost");

        // 2. A pending connection is released when its listener closes.
        let orphan = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
        check!(
            process::linux::dispatch_for_test(42, orphan, addr_ptr, addr_len) == 0,
            "second connect failed"
        );
        check!(task::fd_close(listener as usize), "close listener failed");
        let eof = read_fd(orphan, &mut buf);
        check!(eof == 0, "orphaned client read returned {eof:#x}, expected EOF");

        for fd in [client, server, orphan] {
            check!(task::fd_close(fd as usize), "cleanup close failed");
        }
        check!(fds_clean(), "early-write test left a descriptor");
        check!(unix::bound_count() == 0, "bound name survived its listener");
        check!(pipe::Pipe::live() == 0, "a pending connection leaked a pipe");
        Ok(())
    }

    /// Soak: repeated bind/connect/accept/exchange/close generations, with no
    /// bound-name, descriptor or pipe leak.
    pub fn unix_pathname_soak() -> Result<(), String> {
        fresh()?;
        for round in 0..200u64 {
            let name = alloc::format!("/tmp/abi-soak-{round}.sock");
            let name = name.as_bytes();
            let mut sockaddr = [0u8; 110];
            sockaddr[..2].copy_from_slice(&(AF_UNIX as u16).to_le_bytes());
            sockaddr[2..2 + name.len()].copy_from_slice(name);
            let addr_ptr = sockaddr.as_ptr() as u64;
            let addr_len = (2 + name.len()) as u64;

            let listener = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
            check!(
                process::linux::dispatch_for_test(49, listener, addr_ptr, addr_len) == 0,
                "round {round}: bind failed"
            );
            check!(
                process::linux::dispatch_for_test(50, listener, 1, 0) == 0,
                "round {round}: listen failed"
            );
            let client = process::linux::dispatch_for_test(41, AF_UNIX, SOCK_STREAM, 0);
            let connected = process::linux::dispatch_for_test(42, client, addr_ptr, addr_len);
            check!(
                connected == 0,
                "round {round}: connect returned {connected:#x}"
            );
            let server = process::linux::dispatch_args_for_test(288, listener, 0, 0, 0);
            check!((server as i64) > 0, "round {round}: accept failed");
            check!(write_fd(client, b"x") == 1, "round {round}: write failed");
            let mut byte = [0u8; 1];
            check!(
                read_fd(server, &mut byte) == 1,
                "round {round}: read failed"
            );
            for fd in [client, server, listener] {
                check!(task::fd_close(fd as usize), "round {round}: close failed");
            }
        }
        check!(fds_clean(), "pathname soak leaked a descriptor");
        check!(
            unix::bound_count() == 0,
            "pathname soak leaked a bound name"
        );
        check!(pipe::Pipe::live() == 0, "pathname soak leaked a pipe");
        Ok(())
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

        // SIGKILL/SIGSTOP bits are discarded by rt_sigprocmask. The set is a
        // Linux `sigset_t`, so the bit for `sig` is `1 << (sig - 1)`.
        let mask: u64 = (1 << (signal::SIGKILL - 1))
            | (1 << (signal::SIGSTOP - 1))
            | (1 << (signal::SIGTERM - 1));
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
        // Both masks are in kernel bit order when building the frame.
        let sa_mask = 1u64 << signal::SIGUSR2;
        let saved_mask = (1u64 << signal::SIGUSR1) | (1u64 << signal::SIGTERM);
        let info = SigInfo::fault(signal::SEGV_ACCERR, 0xdead_beef);
        let result = signal::build_linux_frame(
            top,
            &regs,
            signal::SIGSEGV,
            0x0040_2000,
            signal::SA_SIGINFO,
            0x0040_3000,
            sa_mask,
            saved_mask,
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
        // The frame itself carries the masks in Linux `sigset_t` bit order.
        // Safety: `build_linux_frame` just wrote `uc_sigmask` on this stack.
        let raw = unsafe {
            core::ptr::read_volatile((result.rsp + signal::lf::UC_SIGMASK) as *const u64)
        };
        check!(
            raw == signal::kernel_to_linux_sigset(saved_mask),
            "uc_sigmask is {raw:#x}, expected {:#x}",
            signal::kernel_to_linux_sigset(saved_mask)
        );
        // Safety: same frame, just written, at a fixed `sigcontext` offset.
        let raw_old = unsafe {
            core::ptr::read_volatile(
                (result.rsp + signal::lf::MCONTEXT + signal::lf::OLDMASK) as *const u64,
            )
        };
        check!(
            raw_old == signal::kernel_to_linux_sigset(sa_mask),
            "sigcontext.oldmask is {raw_old:#x}, expected {:#x}",
            signal::kernel_to_linux_sigset(sa_mask)
        );
        let (restored, mask) = signal::parse_linux_frame(result.rsp + 8);
        check!(mask == saved_mask, "saved mask is {mask:#x}");
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

    /// The Linux `sigset_t` bit order (`1 << (sig - 1)`) round-trips through
    /// the kernel's internal order (`1 << sig`) for every representable
    /// signal; `SIGRTMAX` has no kernel bit and must not shift out of range.
    pub fn linux_sigset_roundtrip() -> Result<(), String> {
        fresh()?;

        check!(
            signal::linux_sigset_to_kernel(0) == 0 && signal::kernel_to_linux_sigset(0) == 0,
            "the empty set did not translate to 0"
        );
        // Signal 1 is Linux bit 0 and kernel bit 1.
        check!(
            signal::linux_sigset_to_kernel(1) == 1 << signal::SIGHUP,
            "SIGHUP translated to {:#x}",
            signal::linux_sigset_to_kernel(1)
        );
        // Signal 32 is Linux bit 31 and kernel bit 32.
        check!(
            signal::linux_sigset_to_kernel(1 << 31) == 1 << 32,
            "signal 32 translated to {:#x}",
            signal::linux_sigset_to_kernel(1 << 31)
        );
        // Signal 64 (`SIGRTMAX`) is Linux bit 63: it has no kernel bit, so it
        // is dropped rather than shifted out of range.
        check!(
            signal::linux_sigset_to_kernel(1 << 63) == 0,
            "SIGRTMAX translated to {:#x}",
            signal::linux_sigset_to_kernel(1 << 63)
        );
        // Kernel bit 0 is "no signal" and has no Linux bit.
        check!(
            signal::kernel_to_linux_sigset(1) == 0,
            "the kernel's bit 0 leaked into a sigset"
        );
        // Every representable signal round-trips exactly.
        for sig in 1..=63u8 {
            let linux = 1u64 << (sig - 1);
            let kernel = signal::linux_sigset_to_kernel(linux);
            check!(
                kernel == 1u64 << sig,
                "signal {sig} translated to {kernel:#x}"
            );
            check!(
                signal::kernel_to_linux_sigset(kernel) == linux,
                "signal {sig} did not round-trip"
            );
        }
        // Uncatchable bits still translate; the mask filter drops them later.
        let uncatchable = (1u64 << (signal::SIGKILL - 1)) | (1u64 << (signal::SIGSTOP - 1));
        check!(
            signal::linux_sigset_to_kernel(uncatchable)
                == (1u64 << signal::SIGKILL) | (1u64 << signal::SIGSTOP),
            "the uncatchable pair translated to {:#x}",
            signal::linux_sigset_to_kernel(uncatchable)
        );
        signal::harness::reset();
        Ok(())
    }

    /// `rt_sigprocmask` and `rt_sigaction` cross the Linux ABI boundary with
    /// translated masks: a Linux `sigset_t` goes in, the kernel's bit order is
    /// stored, and a Linux `sigset_t` comes back out.
    pub fn linux_sigprocmask_sigset_boundary() -> Result<(), String> {
        fresh()?;
        let me = task::current();

        // SIGUSR2 = 12: Linux bit 11, kernel bit 12.
        let block = 1u64 << (signal::SIGUSR2 - 1);
        let e = process::linux::dispatch_for_test(
            14,
            signal::SIG_BLOCK,
            core::ptr::addr_of!(block) as u64,
            0,
        );
        check!(e == 0, "rt_sigprocmask(SIG_BLOCK) returned {e:#x}");
        check!(
            signal::blocked(me) == 1 << signal::SIGUSR2,
            "kernel blocked mask is {:#x}",
            signal::blocked(me)
        );
        let mut old = 0u64;
        let e = process::linux::dispatch_for_test(14, 0, 0, core::ptr::addr_of_mut!(old) as u64);
        check!(
            e == 0 && old == block,
            "oldset is {old:#x} (ret {e:#x}), expected {block:#x}"
        );

        // SIGKILL/SIGSTOP (Linux bits 8/18) are dropped, never stored raw.
        let uncatchable = (1u64 << (signal::SIGKILL - 1)) | (1u64 << (signal::SIGSTOP - 1));
        let e = process::linux::dispatch_for_test(
            14,
            signal::SIG_BLOCK,
            core::ptr::addr_of!(uncatchable) as u64,
            0,
        );
        check!(e == 0, "blocking SIGKILL/SIGSTOP returned {e:#x}");
        check!(
            signal::blocked(me) & ((1 << signal::SIGKILL) | (1 << signal::SIGSTOP)) == 0,
            "uncatchable bits entered the kernel mask: {:#x}",
            signal::blocked(me)
        );

        // Unblocking in Linux order clears exactly the requested bit.
        let e = process::linux::dispatch_for_test(
            14,
            signal::SIG_UNBLOCK,
            core::ptr::addr_of!(block) as u64,
            0,
        );
        check!(e == 0, "rt_sigprocmask(SIG_UNBLOCK) returned {e:#x}");
        check!(
            signal::blocked(me) == 0,
            "SIGUSR2 stayed blocked: {:#x}",
            signal::blocked(me)
        );

        // SIG_SETMASK with SIGHUP and SIGRTMAX: only SIGHUP is representable.
        let set = 1u64 | (1u64 << 63);
        let e = process::linux::dispatch_for_test(
            14,
            signal::SIG_SETMASK,
            core::ptr::addr_of!(set) as u64,
            0,
        );
        check!(e == 0, "rt_sigprocmask(SIG_SETMASK) returned {e:#x}");
        check!(
            signal::blocked(me) == 1 << signal::SIGHUP,
            "SIG_SETMASK stored {:#x}",
            signal::blocked(me)
        );

        // `rt_sigaction`: `sa_mask` is stored in kernel order (uncatchable bits
        // filtered) and reported back in Linux order.
        let mut action = [0u64; 4];
        action[0] = 0x40_1000;
        action[2] = 0x40_2000;
        action[3] = (1u64 << (signal::SIGUSR2 - 1)) | (1u64 << (signal::SIGKILL - 1));
        let e = process::linux::dispatch_for_test(
            13,
            signal::SIGTERM as u64,
            action.as_ptr() as u64,
            0,
        );
        check!(e == 0, "rt_sigaction(SIGTERM) returned {e:#x}");
        check!(
            signal::action(me, signal::SIGTERM)
                == Disposition::Handler {
                    handler: 0x40_1000,
                    flags: 0,
                    restorer: 0x40_2000,
                    mask: 1 << signal::SIGUSR2,
                },
            "stored action is {:?}",
            signal::action(me, signal::SIGTERM)
        );
        let mut old = [0u64; 4];
        let e = process::linux::dispatch_for_test(
            13,
            signal::SIGTERM as u64,
            0,
            old.as_mut_ptr() as u64,
        );
        check!(
            e == 0 && old[3] == 1 << (signal::SIGUSR2 - 1),
            "reported sa_mask is {:#x} (ret {e:#x})",
            old[3]
        );

        signal::set_blocked(me, 0);
        signal::harness::reset();
        Ok(())
    }

    /// Soak: hundreds of thousands of Linux sets through the translation and
    /// the `rt_sigprocmask` handler, then thousands through the frame builder
    /// and parser, asserting after every cycle that no bit drifted.
    pub fn linux_sigset_translate_soak() -> Result<(), String> {
        fresh()?;
        let me = task::current();
        const ROUNDS: u32 = 500_000;
        let mut seed: u64 = 0x243f_6a88_85a3_08d3;
        let uncatchable = (1u64 << signal::SIGKILL) | (1u64 << signal::SIGSTOP);
        for round in 0..ROUNDS {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let linux = seed;
            let kernel = signal::linux_sigset_to_kernel(linux);
            let back = signal::kernel_to_linux_sigset(kernel);
            // Linux bit 63 (`SIGRTMAX`) has no kernel bit and is dropped.
            let representable = linux & !(1u64 << 63);
            if back != representable {
                return Err(format!(
                    "round {round}: {linux:#018x} -> {kernel:#018x} -> {back:#018x}, expected {representable:#018x}"
                ));
            }

            // The same set through the syscall boundary: block, query back,
            // then clear. The kernel mask must be exactly the translated set
            // minus the uncatchable bits.
            let set = linux;
            let e = process::linux::dispatch_for_test(
                14,
                signal::SIG_BLOCK,
                core::ptr::addr_of!(set) as u64,
                0,
            );
            if e != 0 {
                return Err(format!("round {round}: rt_sigprocmask returned {e:#x}"));
            }
            let expected = kernel & !uncatchable;
            let observed = signal::blocked(me);
            if observed != expected {
                return Err(format!(
                    "round {round}: kernel mask {observed:#018x}, expected {expected:#018x}"
                ));
            }
            let mut old = 0u64;
            let e =
                process::linux::dispatch_for_test(14, 0, 0, core::ptr::addr_of_mut!(old) as u64);
            if e != 0 || old != signal::kernel_to_linux_sigset(expected) {
                return Err(format!(
                    "round {round}: oldset {old:#018x}, expected {:#018x}",
                    signal::kernel_to_linux_sigset(expected)
                ));
            }
            let clear = 0u64;
            let e = process::linux::dispatch_for_test(
                14,
                signal::SIG_SETMASK,
                core::ptr::addr_of!(clear) as u64,
                0,
            );
            if e != 0 || signal::blocked(me) != 0 {
                return Err(format!(
                    "round {round}: clear left {:#x}",
                    signal::blocked(me)
                ));
            }
        }

        // The frame boundary round-trips the same masks without drift.
        let mut stack = vec![0u8; 8192];
        let top = stack.as_mut_ptr() as u64 + stack.len() as u64;
        let regs = signal::UserRegs::default();
        let info = SigInfo::user(0, signal::SI_USER);
        for round in 0..4096u32 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let saved = signal::linux_sigset_to_kernel(seed);
            let result = signal::build_linux_frame(
                top,
                &regs,
                signal::SIGUSR1,
                0x0040_1000,
                0,
                0x0040_2000,
                0,
                saved,
                &info,
            );
            let (_, parsed) = signal::parse_linux_frame(result.rsp + 8);
            // Safety: `build_linux_frame` just wrote `uc_sigmask` on this stack.
            let raw = unsafe {
                core::ptr::read_volatile((result.rsp + signal::lf::UC_SIGMASK) as *const u64)
            };
            if parsed != saved || raw != signal::kernel_to_linux_sigset(saved) {
                return Err(format!(
                    "frame round {round}: saved {saved:#018x}, parsed {parsed:#018x}, uc_sigmask {raw:#018x}"
                ));
            }
        }

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

    /// Spawn `count` client tasks, each holding its own handle to the callable
    /// side `shared` of one channel, as `registry::resolve` hands every client
    /// of a service an alias of the same endpoint. Returns `(slot, handle)`
    /// pairs; `current()` is the kernel task again on return.
    fn shared_clients(shared: u64, count: usize) -> Result<Vec<(usize, u64)>, String> {
        let entry = handles::get(shared).map_err(|error| error.message())?;
        let mut clients = Vec::new();
        for index in 0..count {
            task::harness::switch_current(task::KERNEL_TASK);
            let slot = task::spawn_fork().map_err(|error| format!("client {index}: {error}"))?;
            handles::reset_for_task(slot);
            let handle = handles::open_for_task(slot, entry.kind, entry.rights, entry.object_id)
                .map_err(|error| error.message())?;
            clients.push((slot, handle));
        }
        task::harness::switch_current(task::KERNEL_TASK);
        Ok(clients)
    }

    /// Independent clients calling one service over an aliased endpoint are
    /// not a cycle (the clipboard demo pair hit a false `Deadlock` here): the
    /// second client's call is queued behind the first. Nesting by the same
    /// client and a callback from the service side stay refused while either
    /// call is open, and the callback is allowed once both calls end.
    pub fn concurrent_clients_allowed() -> Result<(), String> {
        fresh()?;
        let (shared, server) = channels::create().map_err(reason)?;
        let clients = shared_clients(shared, 2)?;
        let (first, first_handle) = clients[0];
        let (second, second_handle) = clients[1];
        let request = parcel(7, flags::SYNC, "hello")?;

        task::harness::switch_current(first);
        let first_txn = channels::begin_call(first_handle, 7, &request, None).map_err(reason)?;
        task::harness::switch_current(second);
        let second_txn = channels::begin_call(second_handle, 7, &request, None)
            .map_err(|error| format!("second client refused: {}", error.message()))?;
        check!(first_txn != second_txn, "the two clients share a txn id");

        // Nesting: the first client already has a call open on this channel.
        task::harness::switch_current(first);
        check!(
            channels::begin_call(first_handle, 7, &request, None) == Err(ChannelError::Deadlock),
            "a nested call by the same client was not refused"
        );
        // Callback: the service calls toward the side whose callers are parked.
        task::harness::switch_current(task::KERNEL_TASK);
        check!(
            channels::begin_call(server, 7, &request, None) == Err(ChannelError::Deadlock),
            "a callback toward parked callers was not refused"
        );

        // The service answers both, in arrival order, by transaction id.
        for (slot, txn) in [(first, first_txn), (second, second_txn)] {
            let message = channels::recv(server, None).map_err(reason)?;
            check!(
                message.sender == slot && message.txn == Some(txn),
                "request from {} txn {:?}, expected {slot} txn {txn}",
                message.sender,
                message.txn
            );
            channels::reply(txn, &parcel(8, 0, &format!("to {slot}"))?).map_err(reason)?;
        }
        for (slot, txn) in [(first, first_txn), (second, second_txn)] {
            task::harness::switch_current(slot);
            let got = channels::await_reply(txn).map_err(reason)?;
            check!(
                payload(&got)? == format!("to {slot}"),
                "client {slot} got another client's reply"
            );
        }

        // Idle again: the service may now call its clients' side.
        task::harness::switch_current(task::KERNEL_TASK);
        let back = channels::begin_call(server, 7, &request, None).map_err(reason)?;
        channels::cancel(back).map_err(reason)?;
        check!(
            channels::await_reply(back) == Err(ChannelError::Canceled),
            "callback outcome is not Canceled"
        );
        // Canceling does not dequeue: the callback request is still waiting in
        // the clients' inbox.
        let stats = channels::stats();
        check!(
            stats.calls == 3 && stats.replies == 2 && stats.outstanding == 0 && stats.queued == 1,
            "counters after two clients and a callback: {stats:?}"
        );
        fresh()
    }

    /// Soak: 4 clients call one shared endpoint concurrently for 2000 rounds.
    /// Every round the callback probe is refused, replies go back in reverse
    /// order and each reaches its own caller; nothing is left outstanding,
    /// queued or metered at the end.
    pub fn concurrent_clients_soak() -> Result<(), String> {
        const CLIENTS: usize = 4;
        const ROUNDS: usize = 2000;
        fresh()?;
        let (shared, server) = channels::create().map_err(reason)?;
        let clients = shared_clients(shared, CLIENTS)?;
        let probe = parcel(7, flags::SYNC, "probe")?;
        let start = unsafe { core::arch::x86_64::_rdtsc() };
        for round in 0..ROUNDS {
            let mut txns = Vec::with_capacity(CLIENTS);
            for &(slot, handle) in &clients {
                task::harness::switch_current(slot);
                let request = parcel(7, flags::SYNC, &format!("{round}:{slot}"))?;
                let txn = channels::begin_call(handle, 7, &request, None)
                    .map_err(|error| format!("round {round} client {slot}: {}", error.message()))?;
                txns.push(txn);
            }
            task::harness::switch_current(task::KERNEL_TASK);
            check!(
                channels::begin_call(server, 7, &probe, None) == Err(ChannelError::Deadlock),
                "round {round}: a callback was allowed with calls open"
            );
            let mut inbound = Vec::with_capacity(CLIENTS);
            for _ in 0..CLIENTS {
                inbound.push(channels::recv(server, None).map_err(reason)?);
            }
            for message in inbound.iter().rev() {
                let txn = message
                    .txn
                    .ok_or_else(|| format!("round {round}: a call arrived without a txn"))?;
                let echo = payload(&message.bytes)?;
                channels::reply(txn, &parcel(8, 0, &echo)?).map_err(reason)?;
            }
            for (index, &(slot, _)) in clients.iter().enumerate() {
                task::harness::switch_current(slot);
                let got = channels::await_reply(txns[index]).map_err(reason)?;
                check!(
                    payload(&got)? == format!("{round}:{slot}"),
                    "round {round}: client {slot} got the wrong reply"
                );
            }
        }
        task::harness::switch_current(task::KERNEL_TASK);
        let cycles = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start);
        serial_println!(
            "TEST:ipc_channel_concurrent_clients_soak:INFO:clients={CLIENTS} rounds={ROUNDS} cycles={cycles}"
        );
        let stats = channels::stats();
        let expected = (CLIENTS * ROUNDS) as u64;
        check!(
            stats.calls == expected
                && stats.replies == expected
                && stats.timeouts == 0
                && stats.outstanding == 0
                && stats.queued == 0
                && stats.queued_bytes == 0,
            "counters after the soak: {stats:?}"
        );
        let meters = channels::senders(server).map_err(reason)?;
        check!(
            meters.iter().all(|meter| meter.outstanding == 0),
            "a sender meter still counts open calls: {meters:?}"
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
// Credential transitions (issue #101)
// ---------------------------------------------------------------------------

/// The audited transition gate: only `CAP_SETUID` holders may stamp a task,
/// never toward more privilege, and every attempt reaches the audit ring. The
/// native `creds` syscall is covered by
/// [`credentials_suite::syscall_gate`], and the login service builds on the
/// same API for the real console path.
mod credentials_suite {
    use super::*;
    use crate::ipc::credentials::{self, Cred, TransitionError};
    use crate::ipc::{acl, audit};
    use crate::process::cred_op;

    /// Scratch user space for the syscall test: one page for the request
    /// block, one for the read-back block.
    const SPACE: u64 = 0x0040_0000;
    const SPACE_PAGES: u64 = 2;
    const CRED: u64 = SPACE;
    const OUT: u64 = SPACE + 0x100;

    /// Refusals as the syscall returns them in `rax`.
    const EPERM: i64 = 1;
    const EACCES: i64 = 13;
    const EINVAL: i64 = 22;

    fn failed(code: i64) -> u64 {
        (code as u64).wrapping_neg()
    }

    /// Every transition test starts from the bring-up state: root kernel task,
    /// every slot reset, empty policy, empty audit ring, tracing off.
    fn fresh() {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        for slot in 0..task::MAX_TASKS {
            credentials::reset_for_task(slot);
        }
        acl::load(&[]);
        audit::reset();
        audit::set_trace(false);
    }

    /// Run `f` with [`SPACE`] mapped into a fresh address space installed as
    /// CR3, exactly as a real `creds` syscall from a user task would find it.
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

    /// Write a credential block into the installed scratch space.
    fn write_cred(va: u64, cred: Cred) {
        let words = cred.to_words();
        // Safety: the scratch pages are mapped writable while installed.
        unsafe { core::ptr::copy_nonoverlapping(words.as_ptr(), va as *mut u64, words.len()) };
    }

    /// Read a credential block from the installed scratch space.
    fn read_cred(va: u64) -> Cred {
        let mut words = [0u64; 5];
        // Safety: the scratch pages are mapped readable while installed.
        unsafe { core::ptr::copy_nonoverlapping(va as *const u64, words.as_mut_ptr(), 5) };
        Cred::from_words(words)
    }

    /// A task without `CAP_SETUID` cannot stamp another task; with it, the
    /// same request reaches the target. Both outcomes are audited.
    pub fn transition_requires_cap() -> Result<(), String> {
        fresh();
        let child = task::spawn_child("cred", &service_suite::minimal_elf()).map_err(to_string)?;
        credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
        let before = audit::count();
        let requested = Cred::new(2000, 200, 0, 0, 7);
        check!(
            credentials::transition(task::current(), child, requested)
                == Err(TransitionError::NotPrivileged),
            "a task without CAP_SETUID stamped another task"
        );
        check!(
            credentials::of(child) == Cred::ROOT,
            "the refused stamp changed the target: {:?}",
            credentials::of(child)
        );
        check!(audit::count() == before + 1, "the refusal was not audited");
        let event = *audit::recent(1)
            .first()
            .ok_or("the refusal left no audit event")?;
        check!(
            !event.allow
                && event.interface_id == credentials::AUDIT_INTERFACE
                && event.reason_code == credentials::reason::TRANSITION_NOT_PRIVILEGED
                && event.txn_id == child as u64
                && event.uid == 1000,
            "the refusal record is wrong: {event:?}"
        );

        // With the capability the same request succeeds and reaches the target.
        credentials::set(
            task::current(),
            Cred::new(1000, 100, credentials::CAP_SETUID, 0, 0),
        );
        check!(
            credentials::transition(task::current(), child, requested) == Ok(requested),
            "a CAP_SETUID holder was refused a legal stamp"
        );
        check!(
            credentials::of(child) == requested,
            "the target did not receive the stamp: {:?}",
            credentials::of(child)
        );
        let event = *audit::recent(1)
            .first()
            .ok_or("the allowed stamp left no audit event")?;
        check!(
            event.allow && event.reason_code == credentials::reason::TRANSITION_ALLOWED,
            "the allowed stamp record is wrong: {event:?}"
        );

        // Reading another task's identity is the same privilege; reading your
        // own is always allowed.
        credentials::set(task::current(), Cred::new(1000, 100, 0, 0, 0));
        check!(
            credentials::read(task::current(), child) == Err(TransitionError::NotPrivileged),
            "an unprivileged task read another task's identity"
        );
        check!(
            credentials::read(task::current(), task::current())
                == Ok(credentials::of(task::current())),
            "a task could not read its own identity"
        );
        Ok(())
    }

    /// Widening is refused even with `CAP_SETUID`: uid 0 needs a root actor,
    /// and capability bits never flow up. Refusals are audited, and a downgrade
    /// still works.
    pub fn transition_rejects_widening() -> Result<(), String> {
        fresh();
        let actor = Cred::new(1000, 100, credentials::CAP_SETUID, 0, 0);
        credentials::set(task::current(), actor);
        let before = audit::count();
        check!(
            credentials::transition(task::current(), task::current(), Cred::new(0, 0, 0, 0, 3))
                == Err(TransitionError::Widening),
            "a non-root actor minted uid 0"
        );
        check!(
            credentials::transition(
                task::current(),
                task::current(),
                Cred::new(
                    2000,
                    200,
                    credentials::CAP_SETUID | credentials::CAP_NET_RAW,
                    0,
                    3
                )
            ) == Err(TransitionError::Widening),
            "a transition granted a capability the actor lacks"
        );
        check!(
            credentials::of(task::current()) == actor,
            "a refused widening changed the actor: {:?}",
            credentials::of(task::current())
        );
        check!(
            audit::count() == before + 2,
            "the widening refusals were not audited"
        );
        check!(
            audit::recent(1).first().is_some_and(|event| {
                !event.allow && event.reason_code == credentials::reason::TRANSITION_WIDENING
            }),
            "the last widening refusal is not in the audit ring"
        );

        // Toward less privilege is exactly what the gate is for; keep the
        // capability so the next check reaches the target rule.
        let downgraded = Cred::new(1000, 100, credentials::CAP_SETUID, 4, 9);
        check!(
            credentials::transition(task::current(), task::current(), downgraded) == Ok(downgraded),
            "a legal downgrade was refused"
        );
        check!(
            credentials::of(task::current()) == downgraded,
            "the downgrade did not apply"
        );

        // An unknown target is refused with its own audited reason.
        let before = audit::count();
        check!(
            credentials::transition(task::current(), task::MAX_TASKS, downgraded)
                == Err(TransitionError::BadTarget),
            "an out-of-range target was accepted"
        );
        check!(
            audit::count() == before + 1,
            "the bad-target refusal was not audited"
        );
        check!(
            audit::recent(1).first().is_some_and(|event| {
                !event.allow && event.reason_code == credentials::reason::TRANSITION_BAD_TARGET
            }),
            "the bad-target record is missing"
        );

        // The kernel task may only restamp itself: a service cannot rewrite the
        // multiplexer's identity.
        let child = task::spawn_child("guard", &service_suite::minimal_elf()).map_err(to_string)?;
        task::harness::switch_current(child);
        credentials::set(child, Cred::new(1000, 100, credentials::CAP_SETUID, 0, 0));
        check!(
            credentials::transition(child, task::KERNEL_TASK, Cred::new(1000, 100, 0, 0, 0))
                == Err(TransitionError::BadTarget),
            "a service restamped the kernel task"
        );
        task::harness::switch_current(task::KERNEL_TASK);
        Ok(())
    }

    /// The native gate: `set`/`get` read and write the 40-byte block, and the
    /// refusals arrive as `-errno` (`-EPERM`, `-EACCES`, `-EINVAL`).
    pub fn syscall_gate() -> Result<(), String> {
        fresh();
        in_space(|| -> Result<(), String> {
            let me = task::current();

            // Without the capability the stamp is refused with `-EPERM`.
            credentials::set(me, Cred::new(1000, 100, 0, 0, 0));
            write_cred(CRED, Cred::new(2000, 200, 0, 0, 5));
            let code = process::dispatch_for_test(10, cred_op::SET, me as u64, CRED);
            check!(code == failed(EPERM), "set without the cap -> {code:#x}");
            check!(
                credentials::of(me) == Cred::new(1000, 100, 0, 0, 0),
                "the refused set changed the caller"
            );

            // With it, `set` applies and `get` returns the same block.
            credentials::set(me, Cred::new(1000, 100, credentials::CAP_SETUID, 0, 0));
            let stamped = Cred::new(2000, 200, 0, 0, 5);
            let code = process::dispatch_for_test(10, cred_op::SET, u64::MAX, CRED);
            check!(code == 0, "set with the cap -> {code:#x}");
            check!(
                credentials::of(me) == stamped,
                "the syscall stamp did not apply"
            );
            let code = process::dispatch_for_test(10, cred_op::GET, u64::MAX, OUT);
            check!(code == 0, "get with the cap -> {code:#x}");
            check!(
                read_cred(OUT) == stamped,
                "get returned the wrong block: {:?}",
                read_cred(OUT)
            );

            // Widening is `-EACCES` even with the capability.
            credentials::set(me, Cred::new(1000, 100, credentials::CAP_SETUID, 0, 0));
            write_cred(CRED, Cred::new(0, 0, 0, 0, 6));
            let code = process::dispatch_for_test(10, cred_op::SET, u64::MAX, CRED);
            check!(code == failed(EACCES), "widening set -> {code:#x}");

            // An unknown op is `-EINVAL`.
            let code = process::dispatch_for_test(10, 9, 0, 0);
            check!(code == failed(EINVAL), "unknown op -> {code:#x}");
            Ok(())
        })
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

    pub(crate) fn buffer_reason(error: BufferError) -> String {
        error.message().into()
    }

    pub(crate) fn channel_reason(error: ChannelError) -> String {
        error.message().into()
    }

    fn handle_reason(error: HandleError) -> String {
        error.message().into()
    }

    /// Every shared-buffer test starts from empty registries and a clean kernel
    /// task. `channels::reset` runs first so it can release the buffer
    /// references held by queued messages before the buffers go away.
    pub(crate) fn fresh() -> Result<(), String> {
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
    pub(crate) fn channel_to(slot: usize) -> Result<(u64, u64), String> {
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
    pub(crate) fn parcel_with_transfers(
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
    pub(crate) fn spawn_receiver() -> Result<usize, String> {
        let child = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        handles::reset_for_task(child);
        Ok(child)
    }

    /// Finish and reap `child`, returning to the kernel task and resetting the
    /// task table.
    pub(crate) fn reap(child: usize) -> Result<(), String> {
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
// keyd crypto and SHARE_ONLY key isolation (issue #102)
// ---------------------------------------------------------------------------

/// Runs the same primitives `keyd` links in ring 3 inside the kernel, plus the
/// key-material isolation property: a `SHARE_ONLY` buffer is mapped for its
/// creator and the kernel refuses every other task's `map`.
mod crypto_suite {
    use super::*;
    use crate::ipc::channels;
    use crate::ipc::handles::{self, Error as HandleError};
    use crate::ipc::shared::{self, Error as BufferError};
    use alloc::vec;
    use lazyos_crypto::{hex, hmac, sha256, wrap};

    /// Friendly text for a crypto failure.
    fn crypto_reason(error: lazyos_crypto::Error) -> String {
        error.message().into()
    }

    /// FIPS 180-4 and RFC 4231 vectors, run on the freestanding target so the
    /// exact artifact `keyd` embeds is covered, not just the host build.
    pub fn sha256_hmac_known_answers() -> Result<(), String> {
        let digest = hex::encode(&sha256::sha256(b"abc"));
        check!(
            digest == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "sha256(abc) = {digest}"
        );
        let empty = hex::encode(&sha256::sha256(b""));
        check!(
            empty == "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "sha256(\"\") = {empty}"
        );
        let tag = hex::encode(&hmac::hmac_sha256(&[0x0bu8; 20], b"Hi There"));
        check!(
            tag == "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            "hmac(key, \"Hi There\") = {tag}"
        );
        check!(
            hmac::hmac_sha256_verify(
                &[0x0bu8; 20],
                &[b"Hi There"],
                &hmac::hmac_sha256(&[0x0bu8; 20], b"Hi There")
            ),
            "the constant-time tag check rejected a valid tag"
        );
        Ok(())
    }

    /// A wrap->unwrap round-trip in kernel context, then the same blob handed
    /// to a client inside a `SHARE_ONLY` buffer: the service reads and unwraps
    /// its own mapping first, and the client receives the handle afterwards but
    /// cannot map it.
    pub fn keyd_wrap_roundtrip_share_only() -> Result<(), String> {
        // Part 1: the wrapper round-trips and refuses tampering.
        let key = [0x42u8; 32];
        let nonce = [0x24u8; wrap::NONCE_LEN];
        let secret = b"launch codes: 0000";
        let blob = wrap::wrap_with_nonce(&key, &nonce, secret).map_err(crypto_reason)?;
        let opened = wrap::unwrap(&key, &blob).map_err(crypto_reason)?;
        check!(opened == secret, "wrap round-trip mismatch");
        let mut tampered = blob.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        check!(
            wrap::unwrap(&key, &tampered) == Err(lazyos_crypto::Error::BadTag),
            "a tampered blob unwrapped"
        );

        // Part 2: the SHARE_ONLY handoff. `fresh` mirrors
        // `buffer_share_only_not_mappable`: the registry starts empty.
        ipc_shared_suite::fresh()?;
        let creator = task::current();
        let child = ipc_shared_suite::spawn_receiver()?;
        let (client, child_server) = ipc_shared_suite::channel_to(child)?;
        let handle = shared::create(
            blob.len() as u64,
            shared::flags::READ | shared::flags::WRITE | shared::flags::SHARE_ONLY,
        )
        .map_err(ipc_shared_suite::buffer_reason)?;
        let creator_va = shared::map(handle).map_err(ipc_shared_suite::buffer_reason)?;
        // The creator (standing in for `keyd`) writes the wrapped blob through
        // its own mapping and can read it back: material at rest is visible
        // only to the service.
        for (offset, byte) in blob.iter().enumerate() {
            // Safety: the creator's mapping is writable for the buffer size.
            unsafe { (creator_va as *mut u8).add(offset).write_volatile(*byte) };
        }
        let mut readback = vec![0u8; blob.len()];
        for (offset, slot) in readback.iter_mut().enumerate() {
            // Safety: the creator's mapping is readable for the buffer size.
            *slot = unsafe { (creator_va as *const u8).add(offset).read_volatile() };
        }
        check!(readback == blob, "the service's own mapping changed");
        let opened = wrap::unwrap(&key, &readback).map_err(crypto_reason)?;
        check!(
            opened == secret,
            "the service could not unwrap its own blob"
        );

        // Hand the handle to the client. The transfer moves the handle and its
        // only mapping out of the creator; the client gets the handle but the
        // kernel refuses to map it, so no client address space ever sees the
        // blob.
        let bytes =
            ipc_shared_suite::parcel_with_transfers(1, "wrapped key", vec![handle], Vec::new())?;
        channels::send(client, &bytes).map_err(ipc_shared_suite::channel_reason)?;
        check!(
            handles::get(handle) == Err(HandleError::InvalidHandle),
            "the transfer did not move the sender's handle"
        );
        check!(
            raw_entry(mem::kernel_table(), creator_va).is_none(),
            "the creator's mapping outlived the handle transfer"
        );

        task::harness::switch_current(child);
        let message = channels::try_recv(child_server)
            .map_err(ipc_shared_suite::channel_reason)?
            .ok_or("the transferred message is missing")?;
        check!(
            message.handles.len() == 1,
            "delivered {} handles, expected 1",
            message.handles.len()
        );
        check!(
            shared::map(message.handles[0]) == Err(BufferError::ShareOnly),
            "a client mapped a SHARE_ONLY key buffer"
        );

        // Cleanup in the same order as the other shared-buffer tests: the
        // registries go first so the queued message's references are released.
        shared::reset();
        handles::reset_for_task(child);
        task::harness::switch_current(creator);
        channels::reset();
        ipc_shared_suite::reap(child)?;
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

// ---------------------------------------------------------------------------
// Block devices (issue #100)
// ---------------------------------------------------------------------------

mod block_suite {
    use super::*;
    use crate::block::{self, BlockDevice, BlockError, SECTOR_SIZE};
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicU32, Ordering};
    use spin::Mutex;

    /// An in-memory [`BlockDevice`]: pins the trait's read/write/flush/bounds
    /// contract without hardware. Leaked so the registry can hold it forever.
    /// The ext2 suite reuses it as the backing store for formatted images.
    pub(super) struct FakeDisk {
        name: &'static str,
        pub(super) data: Mutex<Vec<u8>>,
        writes: AtomicU32,
        pub(super) flushes: AtomicU32,
    }

    impl FakeDisk {
        pub(super) fn new(name: &'static str, sectors: usize) -> &'static FakeDisk {
            Box::leak(Box::new(FakeDisk {
                name,
                data: Mutex::new(vec![0u8; sectors * SECTOR_SIZE]),
                writes: AtomicU32::new(0),
                flushes: AtomicU32::new(0),
            }))
        }
    }

    impl BlockDevice for FakeDisk {
        fn name(&self) -> &'static str {
            self.name
        }

        fn sector_count(&self) -> u64 {
            (self.data.lock().len() / SECTOR_SIZE) as u64
        }

        fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
            self.check_range(lba, buf.len())?;
            let data = self.data.lock();
            let start = lba as usize * SECTOR_SIZE;
            buf.copy_from_slice(&data[start..start + buf.len()]);
            Ok(())
        }

        fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
            self.check_range(lba, buf.len())?;
            let mut data = self.data.lock();
            let start = lba as usize * SECTOR_SIZE;
            data[start..start + buf.len()].copy_from_slice(buf);
            self.writes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn flush(&self) -> Result<(), BlockError> {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn is_writable(&self) -> bool {
            true
        }
    }

    /// The trait's data path on a fake device: write, read back, flush, and
    /// the bounds/misalignment errors.
    pub fn fake_read_write_flush() -> Result<(), String> {
        let disk = FakeDisk::new("test-fake0", 8);
        check!(
            block::register(disk).is_ok(),
            "registering the fake disk failed"
        );
        check!(
            disk.sector_size() == SECTOR_SIZE && disk.sector_count() == 8,
            "fake geometry is {} sectors of {}, expected 8 of {SECTOR_SIZE}",
            disk.sector_count(),
            disk.sector_size()
        );
        check!(disk.is_writable(), "the fake disk claims to be read-only");

        let mut sector = [0u8; SECTOR_SIZE];
        for (index, byte) in sector.iter_mut().enumerate() {
            *byte = (index as u8) ^ 0x5A;
        }
        disk.write_sectors(3, &sector)
            .map_err(|error| format!("write failed: {error:?}"))?;
        check!(
            disk.writes.load(Ordering::Relaxed) == 1,
            "the write counter did not move"
        );
        let mut readback = [0u8; SECTOR_SIZE];
        disk.read_sectors(3, &mut readback)
            .map_err(|error| format!("read failed: {error:?}"))?;
        check!(
            readback == sector,
            "read back different bytes than were written"
        );
        disk.flush()
            .map_err(|error| format!("flush failed: {error:?}"))?;
        check!(
            disk.flushes.load(Ordering::Relaxed) == 1,
            "flush did not reach the device"
        );

        check!(
            disk.read_sectors(8, &mut readback).err() == Some(BlockError::Bounds),
            "reading past the last sector was not Bounds"
        );
        check!(
            disk.read_sectors(7, &mut [0u8; 2 * SECTOR_SIZE]).err() == Some(BlockError::Bounds),
            "a range straddling the end was not Bounds"
        );
        check!(
            disk.write_sectors(0, &[0u8; 100]).err() == Some(BlockError::Unsupported),
            "a partial sector was not Unsupported"
        );
        check!(
            disk.read_sectors(0, &mut []).is_ok(),
            "an empty transfer failed"
        );
        Ok(())
    }

    /// Registration, lookup by name, duplicate rejection, listing, and the
    /// boot-device selection.
    pub fn registry_register_lookup_duplicate() -> Result<(), String> {
        let first = FakeDisk::new("test-registry-a", 4);
        let second = FakeDisk::new("test-registry-b", 4);
        check!(block::register(first).is_ok(), "registering a failed");
        check!(block::register(second).is_ok(), "registering b failed");
        check!(
            block::register(first).err() == Some(BlockError::Exists),
            "a duplicate device name was accepted"
        );
        check!(
            block::device("test-registry-a").map(|dev| dev.name()) == Some("test-registry-a"),
            "lookup by name failed"
        );
        check!(
            block::device("test-registry-missing").is_none(),
            "an unknown device name matched"
        );
        let names: Vec<&str> = block::devices().iter().map(|dev| dev.name()).collect();
        check!(
            names.contains(&"test-registry-a") && names.contains(&"test-registry-b"),
            "devices() is {names:?}"
        );
        block::set_boot_device(first);
        check!(
            block::boot_device().map(|dev| dev.name()) == Some("test-registry-a"),
            "set_boot_device did not stick"
        );
        Ok(())
    }

    /// The ATA path still reads the boot disk through the trait: sector 0
    /// carries a valid MBR with a FAT partition, and that partition's boot
    /// sector is a 512-byte-per-sector BPB. This is the acceptance for "the
    /// default image boots from ATA through the block layer".
    pub fn ata_reads_fat_root() -> Result<(), String> {
        block::init();
        let device = block::device("ata0").ok_or_else(|| String::from("ata0 is not registered"))?;
        check!(device.sector_count() > 0, "ata0 reports an empty geometry");

        let mut mbr = [0u8; SECTOR_SIZE];
        device
            .read_sectors(0, &mut mbr)
            .map_err(|error| format!("MBR read failed: {error:?}"))?;
        check!(
            mbr[510] == 0x55 && mbr[511] == 0xAA,
            "sector 0 has no MBR signature"
        );

        let mut fat_lba = None;
        for index in 0..4 {
            let base = 0x1BE + index * 16;
            let kind = mbr[base + 4];
            let start =
                u32::from_le_bytes([mbr[base + 8], mbr[base + 9], mbr[base + 10], mbr[base + 11]]);
            let sectors = u32::from_le_bytes([
                mbr[base + 12],
                mbr[base + 13],
                mbr[base + 14],
                mbr[base + 15],
            ]);
            let is_fat = matches!(kind, 0x01 | 0x04 | 0x06 | 0x0B | 0x0C);
            if is_fat && sectors > 0 {
                fat_lba = Some(start);
                break;
            }
        }
        let lba = fat_lba.ok_or_else(|| String::from("the MBR carries no FAT partition"))?;

        let mut bpb = [0u8; SECTOR_SIZE];
        device
            .read_sectors(u64::from(lba), &mut bpb)
            .map_err(|error| format!("BPB read failed: {error:?}"))?;
        let bytes_per_sector = u16::from_le_bytes([bpb[11], bpb[12]]);
        check!(
            bytes_per_sector == SECTOR_SIZE as u16,
            "the BPB says {bytes_per_sector} bytes per sector"
        );

        // The `mount <dev>` surface: the boot device can be mounted at a new
        // point; duplicate points and non-boot devices are refused. This also
        // proves the filesystem layer reached the disk through the registry.
        check!(
            crate::fs::init(),
            "the FAT volume did not mount through the block layer"
        );
        check!(
            crate::fs::mount_device("/mnt", "ata0").is_ok(),
            "mounting ata0 at /mnt failed"
        );
        check!(
            crate::fs::mount_device("/mnt", "ata0").err() == Some(crate::fs::vfs::FsError::Exists),
            "a duplicate mount point was accepted"
        );
        check!(
            crate::fs::mount_device("/mnt2", "test-registry-a").err()
                == Some(crate::fs::vfs::FsError::NotSupported),
            "a non-boot device was mounted"
        );
        check!(
            crate::fs::mount_device("/mnt3", "no-such-device").err()
                == Some(crate::fs::vfs::FsError::NotFound),
            "an unknown device was mounted"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// VFS core (issue #98)
// ---------------------------------------------------------------------------

mod fs_suite {
    use super::*;
    use crate::fs::ramfs::RamFs;
    use crate::fs::vfs::{self, FileKind, FsError, Id, Meta, Path, Vfs};
    use alloc::sync::Arc;

    /// A fresh VFS with ramfs mounted at `/`.
    fn ram_vfs() -> Vfs {
        let mut vfs = Vfs::new();
        vfs.mount("/", Arc::new(RamFs::new()))
            .expect("mount ramfs at /");
        vfs
    }

    /// Friendly, debuggable conversion for `?` in tests.
    fn fs_error(error: FsError) -> String {
        format!("{} ({error:?})", error.message())
    }

    /// `Path` folds `.`/`..` lexically (clamped at the root), collapses
    /// slashes, and roots relative inputs; mounts resolve by longest prefix,
    /// and `..` folds before mount lookup.
    pub fn path_resolution_and_mounts() -> Result<(), String> {
        for (raw, expected) in [
            ("/", "/"),
            (".", "/"),
            ("/..", "/"),
            ("/a/../..", "/"),
            ("/a/./b/../c", "/a/c"),
            ("//a//b/", "/a/b"),
            ("a/b", "/a/b"),
        ] {
            let folded = Path::parse(raw).to_path_string();
            check!(
                folded == expected,
                "{raw:?} folded to {folded:?}, expected {expected:?}"
            );
        }
        check!(
            Path::parse("/a").is_absolute() && !Path::parse("a").is_absolute(),
            "absolute detection is wrong"
        );
        check!(
            Path::parse("/a/b").name() == Some("b") && Path::parse("/").name().is_none(),
            "the final component is wrong"
        );

        let root = Id::ROOT;
        let mut vfs = Vfs::new();
        vfs.mount("/", Arc::new(RamFs::new())).map_err(fs_error)?;
        vfs.mount("/tmp", Arc::new(RamFs::new()))
            .map_err(fs_error)?;

        // A file written under /tmp lands in the /tmp filesystem, not the root.
        vfs.create(root, "/tmp/scratch.txt", 0o644)
            .map_err(fs_error)?;
        vfs.create(root, "/hello.txt", 0o644).map_err(fs_error)?;
        check!(
            vfs.stat(root, "/tmp/scratch.txt").is_ok(),
            "/tmp/scratch.txt is missing"
        );
        check!(
            vfs.stat(root, "/hello.txt").is_ok(),
            "/hello.txt is missing"
        );
        check!(
            vfs.stat(root, "/scratch.txt").err() == Some(FsError::NotFound),
            "/scratch.txt leaked out of the /tmp mount"
        );

        // The longest mount point wins: /tmp/nested is its own filesystem.
        vfs.mount("/tmp/nested", Arc::new(RamFs::new()))
            .map_err(fs_error)?;
        vfs.create(root, "/tmp/nested/inner.txt", 0o644)
            .map_err(fs_error)?;
        check!(
            vfs.stat(root, "/tmp/nested/inner.txt").is_ok(),
            "the deepest mount did not receive the file"
        );
        check!(
            vfs.stat(root, "/tmp/inner.txt").err() == Some(FsError::NotFound),
            "the nested mount leaked into /tmp"
        );

        // `..` folds before mount resolution: /tmp/../hello.txt is the root fs.
        check!(
            vfs.stat(root, "/tmp/../hello.txt").is_ok(),
            ".. did not fold to the root"
        );
        check!(
            vfs.stat(root, "/tmp/../scratch.txt").err() == Some(FsError::NotFound),
            ".. resolved inside the /tmp mount"
        );

        let mounts = vfs.mounts();
        check!(
            mounts
                .iter()
                .any(|(point, name)| point == "/tmp" && *name == "ramfs"),
            "mount table is {mounts:?}"
        );
        Ok(())
    }

    /// ramfs: create/write (at offsets), read, stat, readdir, rename, unlink,
    /// error cases, and the umask applied at creation.
    pub fn ramfs_create_write_read_rename_unlink() -> Result<(), String> {
        let root = Id::ROOT;
        let mut vfs = ram_vfs();

        vfs.mkdir(root, "/docs", 0o755).map_err(fs_error)?;
        vfs.create(root, "/docs/note.txt", 0o644)
            .map_err(fs_error)?;
        let meta = vfs.stat(root, "/docs/note.txt").map_err(fs_error)?;
        check!(
            meta.kind == FileKind::File
                && meta.size == 0
                && meta.mode & vfs::S_IFMT == vfs::S_IFREG,
            "fresh file metadata is {meta:?}"
        );

        check!(
            vfs.write(root, "/docs/note.txt", 0, b"hello")
                .map_err(fs_error)?
                == 5,
            "first write was short"
        );
        vfs.write(root, "/docs/note.txt", 5, b" world")
            .map_err(fs_error)?;
        let data = vfs.read_file(root, "/docs/note.txt").map_err(fs_error)?;
        check!(data == b"hello world".to_vec(), "contents are {data:?}");
        check!(
            vfs.stat(root, "/docs/note.txt").map_err(fs_error)?.size == 11,
            "stat did not see the appended bytes"
        );

        let mut buf = [0u8; 4];
        let read = vfs
            .read(root, "/docs/note.txt", 6, &mut buf)
            .map_err(fs_error)?;
        check!(
            read == 4 && &buf == b"worl",
            "offset read got {read} bytes {buf:?}"
        );
        check!(
            vfs.read(root, "/docs/note.txt", 99, &mut buf)
                .map_err(fs_error)?
                == 0,
            "read past EOF did not return 0"
        );

        let names: Vec<String> = vfs
            .readdir(root, "/docs")
            .map_err(fs_error)?
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        check!(names == ["note.txt"], "readdir is {names:?}");

        vfs.rename(root, "/docs/note.txt", "/docs/memo.txt")
            .map_err(fs_error)?;
        check!(
            vfs.stat(root, "/docs/note.txt").err() == Some(FsError::NotFound),
            "rename left the source behind"
        );
        check!(
            vfs.read_file(root, "/docs/memo.txt").map_err(fs_error)? == b"hello world".to_vec(),
            "rename lost the contents"
        );

        vfs.unlink(root, "/docs/memo.txt").map_err(fs_error)?;
        check!(
            vfs.stat(root, "/docs/memo.txt").err() == Some(FsError::NotFound),
            "unlink left the file behind"
        );
        check!(
            vfs.readdir(root, "/docs").map_err(fs_error)?.is_empty(),
            "readdir still lists the unlinked file"
        );

        check!(
            vfs.unlink(root, "/nope").err() == Some(FsError::NotFound),
            "unlink found a ghost"
        );
        check!(
            vfs.mkdir(root, "/docs", 0o755).err() == Some(FsError::Exists),
            "mkdir overwrote a dir"
        );
        check!(
            vfs.create(root, "/missing/file", 0o644).err() == Some(FsError::NotFound),
            "create succeeded in a missing directory"
        );
        check!(
            vfs.unlink(root, "/docs").err() == Some(FsError::IsDir),
            "unlink removed a directory"
        );
        check!(
            vfs.write(root, "/docs", 0, b"x").err() == Some(FsError::IsDir),
            "write succeeded on a directory"
        );

        // The umask masks creation modes (`umask(2)` semantics).
        let previous = vfs.set_umask(0o077);
        check!(
            previous == 0o022,
            "default umask is {previous:o}, expected 022"
        );
        vfs.create(root, "/private.txt", 0o666).map_err(fs_error)?;
        let meta = vfs.stat(root, "/private.txt").map_err(fs_error)?;
        check!(
            meta.mode & 0o777 == 0o600,
            "umask left mode {:o}, expected 600",
            meta.mode & 0o777
        );
        check!(vfs.umask() == 0o077, "umask readback is {:o}", vfs.umask());
        Ok(())
    }

    /// The owner/group/other matrix against kernel-stamped ids, root bypass,
    /// `F_OK`, and directory traversal through a real VFS.
    pub fn permission_matrix_owner_group_other() -> Result<(), String> {
        let file = Meta {
            ino: 5,
            mode: vfs::S_IFREG | 0o640,
            uid: 1000,
            gid: 100,
            size: 0,
            kind: FileKind::File,
        };
        let owner = Id::new(1000, 200);
        let group = Id::new(2000, 100);
        let other = Id::new(2000, 200);

        check!(
            vfs::check_access(&file, owner, vfs::READ | vfs::WRITE).is_ok(),
            "the owner was denied rw"
        );
        check!(
            vfs::check_access(&file, owner, vfs::EXECUTE).err() == Some(FsError::Access),
            "the owner was allowed x"
        );
        check!(
            vfs::check_access(&file, group, vfs::READ).is_ok(),
            "the group was denied r"
        );
        check!(
            vfs::check_access(&file, group, vfs::WRITE).err() == Some(FsError::Access),
            "the group was allowed w"
        );
        check!(
            vfs::check_access(&file, other, vfs::READ).err() == Some(FsError::Access),
            "other was allowed r"
        );
        check!(
            vfs::check_access(&file, Id::ROOT, vfs::READ | vfs::WRITE | vfs::EXECUTE).is_ok(),
            "root did not bypass the mode bits"
        );
        check!(
            vfs::check_access(&file, other, 0).is_ok(),
            "an F_OK-style check failed"
        );

        // Through a VFS: a 0700 directory hides its contents from everyone but
        // its owner (and root), even when the file inside is world-readable.
        let mut vfs = ram_vfs();
        vfs.mkdir(Id::ROOT, "/home", 0o700).map_err(fs_error)?;
        vfs.create(Id::ROOT, "/home/secret", 0o644)
            .map_err(fs_error)?;
        check!(
            vfs.stat(Id::ROOT, "/home/secret").is_ok(),
            "root could not stat inside /home"
        );
        check!(
            vfs.stat(group, "/home/secret").err() == Some(FsError::Access),
            "the group traversed a 0700 directory"
        );
        check!(
            vfs.stat(other, "/home/secret").err() == Some(FsError::Access),
            "other traversed a 0700 directory"
        );

        // A world-readable file on a traversable path: read allowed, write not.
        vfs.mkdir(Id::ROOT, "/public", 0o755).map_err(fs_error)?;
        vfs.create(Id::ROOT, "/public/readme", 0o644)
            .map_err(fs_error)?;
        check!(
            vfs.read_file(other, "/public/readme").is_ok(),
            "the world could not read a 0644 file"
        );
        check!(
            vfs.write(other, "/public/readme", 0, b"x").err() == Some(FsError::Access),
            "the world could write a 0644 file"
        );
        Ok(())
    }

    /// The sticky bit on shared directories: entry owner, directory owner, or
    /// root may unlink/rename; others cannot. Also the pure rule function.
    pub fn traversal_and_sticky_bits() -> Result<(), String> {
        let dir = Meta {
            ino: 6,
            mode: vfs::S_IFDIR | 0o1777,
            uid: 3000,
            gid: 300,
            size: 0,
            kind: FileKind::Dir,
        };
        let entry = Meta {
            ino: 7,
            mode: vfs::S_IFREG | 0o644,
            uid: 1000,
            gid: 100,
            size: 0,
            kind: FileKind::File,
        };
        check!(
            vfs::check_sticky(&dir, &entry, Id::new(3000, 1)).is_ok(),
            "the directory owner was denied"
        );
        check!(
            vfs::check_sticky(&dir, &entry, Id::new(1000, 1)).is_ok(),
            "the entry owner was denied"
        );
        check!(
            vfs::check_sticky(&dir, &entry, Id::new(2000, 1)).err() == Some(FsError::Access),
            "a stranger was allowed"
        );
        check!(
            vfs::check_sticky(&dir, &entry, Id::ROOT).is_ok(),
            "root was denied"
        );
        let normal = Meta {
            mode: vfs::S_IFDIR | 0o777,
            ..dir
        };
        check!(
            vfs::check_sticky(&normal, &entry, Id::new(2000, 1)).is_ok(),
            "a non-sticky directory restricted unlink"
        );

        // End to end: alice and bob share a sticky directory.
        let mut vfs = ram_vfs();
        // The default umask would clear the shared directory's world-write bit.
        vfs.set_umask(0);
        let alice = Id::new(1000, 100);
        let bob = Id::new(2000, 200);
        vfs.mkdir(Id::ROOT, "/shared", 0o1777).map_err(fs_error)?;
        vfs.create(alice, "/shared/alice.txt", 0o644)
            .map_err(fs_error)?;
        check!(
            vfs.unlink(bob, "/shared/alice.txt").err() == Some(FsError::Access),
            "bob removed alice's sticky entry"
        );
        check!(
            vfs.rename(bob, "/shared/alice.txt", "/shared/stolen.txt")
                .err()
                == Some(FsError::Access),
            "bob renamed alice's sticky entry"
        );
        check!(
            vfs.unlink(alice, "/shared/alice.txt").is_ok(),
            "alice could not remove her own entry"
        );
        vfs.create(alice, "/shared/alice2.txt", 0o644)
            .map_err(fs_error)?;
        check!(
            vfs.unlink(Id::ROOT, "/shared/alice2.txt").is_ok(),
            "root was sticky-blocked"
        );

        // A sticky directory owned by alice: she may remove bob's entry.
        vfs.mkdir(alice, "/shared/alice-dir", 0o1777)
            .map_err(fs_error)?;
        vfs.create(bob, "/shared/alice-dir/bob.txt", 0o644)
            .map_err(fs_error)?;
        check!(
            vfs.unlink(alice, "/shared/alice-dir/bob.txt").is_ok(),
            "the sticky directory owner could not remove an entry"
        );
        Ok(())
    }

    /// Caches serve repeated lookups, mutations refresh sizes, and unlink or
    /// rename invalidates the entry (and a directory's cached descendants).
    pub fn cache_invalidation() -> Result<(), String> {
        let root = Id::ROOT;
        let mut vfs = ram_vfs();
        vfs.create(root, "/cache.txt", 0o644).map_err(fs_error)?;

        let first = vfs.stat(root, "/cache.txt").map_err(fs_error)?;
        let second = vfs.stat(root, "/cache.txt").map_err(fs_error)?;
        check!(first == second, "the two stats disagree");
        let stats = vfs.cache_stats();
        check!(
            stats.dentry_hits >= 1 && stats.inode_hits >= 1,
            "the caches did not warm: {stats:?}"
        );

        vfs.write(root, "/cache.txt", 0, b"123456")
            .map_err(fs_error)?;
        check!(
            vfs.stat(root, "/cache.txt").map_err(fs_error)?.size == 6,
            "the cached size is stale after a write"
        );

        vfs.unlink(root, "/cache.txt").map_err(fs_error)?;
        check!(
            vfs.stat(root, "/cache.txt").err() == Some(FsError::NotFound),
            "the unlinked entry is still cached"
        );
        check!(
            vfs.cache_stats().invalidations >= 1,
            "no invalidation was recorded"
        );

        // Renaming a directory drops its cached descendants: a stale path must
        // not keep resolving.
        vfs.mkdir(root, "/dir", 0o755).map_err(fs_error)?;
        vfs.create(root, "/dir/file", 0o644).map_err(fs_error)?;
        check!(
            vfs.stat(root, "/dir/file").is_ok(),
            "subtree did not resolve before rename"
        );
        vfs.rename(root, "/dir", "/dir2").map_err(fs_error)?;
        check!(
            vfs.stat(root, "/dir/file").err() == Some(FsError::NotFound),
            "a stale descendant survived the rename"
        );
        check!(
            vfs.stat(root, "/dir2/file").is_ok(),
            "the renamed subtree is missing"
        );

        // invalidate() is the explicit escape hatch and the next lookup refills.
        vfs.invalidate("/dir2/file");
        check!(
            vfs.stat(root, "/dir2/file").is_ok(),
            "the refill after invalidate failed"
        );
        Ok(())
    }

    /// The FAT boot volume is mounted at `/` through the VFS: reads work, and
    /// every mutating call answers EROFS with the friendly message.
    pub fn fat_read_only_erofs() -> Result<(), String> {
        task::register_kernel();
        check!(
            crate::fs::init(),
            "the FAT boot volume did not mount (is the disk image attached?)"
        );
        let root = Id::ROOT;
        let meta = crate::fs::vfs_stat(root, "/HELLO.TXT").map_err(fs_error)?;
        check!(
            meta.kind == FileKind::File && meta.size > 0,
            "HELLO.TXT metadata is {meta:?}"
        );
        let data = crate::fs::vfs_read(root, "/HELLO.TXT").map_err(fs_error)?;
        check!(
            data.windows(17)
                .any(|window| window == b"Hello from LazyOS"),
            "HELLO.TXT contents are wrong"
        );
        let listing = crate::fs::list();
        check!(
            listing
                .iter()
                .any(|(name, is_dir, size)| name == "HELLO.TXT" && !is_dir && *size > 0),
            "the root listing is {listing:?}"
        );

        // The global umask is readable back (the `umask(2)` surface).
        let previous = crate::fs::vfs_set_umask(0o027);
        check!(
            crate::fs::vfs_umask() == 0o027,
            "the global umask did not stick"
        );
        check!(
            crate::fs::vfs_set_umask(previous) == 0o027,
            "umask did not return the previous value"
        );

        check!(
            crate::fs::vfs_write(root, "/HELLO.TXT", 0, b"x").err() == Some(FsError::ReadOnly),
            "a FAT write was not EROFS"
        );
        check!(
            crate::fs::vfs_create(root, "/NEW.TXT", 0o644).err() == Some(FsError::ReadOnly),
            "a FAT create was not EROFS"
        );
        check!(
            crate::fs::vfs_mkdir(root, "/newdir", 0o755).err() == Some(FsError::ReadOnly),
            "a FAT mkdir was not EROFS"
        );
        check!(
            crate::fs::vfs_unlink(root, "/HELLO.TXT").err() == Some(FsError::ReadOnly),
            "a FAT unlink was not EROFS"
        );
        check!(
            crate::fs::vfs_rename(root, "/HELLO.TXT", "/HI.TXT").err() == Some(FsError::ReadOnly),
            "a FAT rename was not EROFS"
        );
        check!(
            FsError::ReadOnly.message().contains("read-only"),
            "the EROFS message is not friendly: {:?}",
            FsError::ReadOnly.message()
        );
        Ok(())
    }

    /// The Linux fd layer routes `openat`/`getdents64`/`fstat`/`close` through
    /// the VFS: a ramfs directory on `/tmp` lists its real entries.
    pub fn getdents64_ramfs_directory() -> Result<(), String> {
        task::register_kernel();
        crate::fs::init();
        let root = Id::ROOT;
        let dir = "/tmp/vfs-getdents";
        let _ = crate::fs::vfs_unlink(root, "/tmp/vfs-getdents/entry.txt");
        crate::fs::vfs_mkdir(root, dir, 0o755).map_err(fs_error)?;
        crate::fs::vfs_create(root, "/tmp/vfs-getdents/entry.txt", 0o644).map_err(fs_error)?;

        let path = b"/tmp/vfs-getdents\0";
        // dispatch_for_test(nr, a1, a2, a3): openat's a1 is `dirfd` (ignored)
        // and a2 is the path.
        let fd = process::linux::dispatch_for_test(257, 0, path.as_ptr() as u64, 0);
        check!(
            (3..task::FD_COUNT as u64).contains(&fd),
            "openat returned {fd:#x}"
        );

        let mut buf = [0u8; 512];
        let count =
            process::linux::dispatch_for_test(217, fd, buf.as_mut_ptr() as u64, buf.len() as u64);
        check!(
            count > 0 && count as usize <= buf.len(),
            "getdents64 returned {count}"
        );

        let mut names: Vec<String> = Vec::new();
        let mut offset = 0usize;
        while offset < count as usize {
            let reclen = u16::from_le_bytes([buf[offset + 16], buf[offset + 17]]) as usize;
            check!(
                reclen >= 19 && offset + reclen <= count as usize,
                "bad dirent record at offset {offset}"
            );
            let name = &buf[offset + 19..offset + reclen];
            let end = name
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(name.len());
            names.push(String::from_utf8_lossy(&name[..end]).into_owned());
            offset += reclen;
        }
        check!(
            names.iter().any(|name| name == "."),
            "no `.` entry: {names:?}"
        );
        check!(
            names.iter().any(|name| name == ".."),
            "no `..` entry: {names:?}"
        );
        check!(
            names.iter().any(|name| name == "entry.txt"),
            "no entry.txt in the listing: {names:?}"
        );

        // fstat on the directory fd reports the VFS directory mode.
        let mut stat = [0u8; 144];
        let result = process::linux::dispatch_for_test(5, fd, stat.as_mut_ptr() as u64, 0);
        check!(result == 0, "fstat -> {result:#x}");
        let mode = u32::from_le_bytes([stat[24], stat[25], stat[26], stat[27]]);
        check!(
            mode & vfs::S_IFMT as u32 == 0o040000,
            "fstat mode is {mode:#o}, expected a directory"
        );

        let result = process::linux::dispatch_for_test(3, fd, 0, 0);
        check!(result == 0, "close -> {result:#x}");
        crate::fs::vfs_unlink(root, "/tmp/vfs-getdents/entry.txt").map_err(fs_error)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Linux ABI copy-up overlay root (issue #136)
// ---------------------------------------------------------------------------

mod overlay_suite {
    use super::*;
    use crate::fs::overlay::Overlay;
    use crate::fs::ramfs::RamFs;
    use crate::fs::vfs::{self, FileKind, Filesystem, FsError, Id, Vfs};
    use alloc::sync::Arc;

    /// Friendly, debuggable conversion for `?` in tests.
    fn fs_error(error: FsError) -> String {
        format!("{} ({error:?})", error.message())
    }

    /// Lower-layer fixture standing in for the read-only FAT volume: the
    /// overlay never calls a mutating method on it, so a ramfs works and lets
    /// the tests prove the lower bytes stay untouched.
    fn lower_fixture() -> Result<Arc<RamFs>, String> {
        let lower = Arc::new(RamFs::new());
        for (name, data) in [
            ("HELLO.TXT", b"Hello from LazyOS".as_slice()),
            ("LOWER.TXT", b"lower".as_slice()),
        ] {
            lower.create(name, 0o555, Id::ROOT).map_err(fs_error)?;
            lower.write(name, 0, data).map_err(fs_error)?;
        }
        lower.mkdir("LDIR", 0o555, Id::ROOT).map_err(fs_error)?;
        lower
            .create("LDIR/INNER.TXT", 0o555, Id::ROOT)
            .map_err(fs_error)?;
        lower
            .write("LDIR/INNER.TXT", 0, b"inner")
            .map_err(fs_error)?;
        Ok(lower)
    }

    /// An overlay-mounted VFS plus the handle to inspect its usage.
    fn mounted_overlay(lower: Arc<RamFs>) -> Result<(Vfs, Arc<Overlay>), String> {
        let overlay = Arc::new(Overlay::new(lower));
        let mut vfs = Vfs::new();
        vfs.mount("/", overlay.clone()).map_err(fs_error)?;
        Ok((vfs, overlay))
    }

    /// Read a whole lower file directly, bypassing the overlay.
    fn lower_bytes(lower: &RamFs, path: &str) -> Result<Vec<u8>, String> {
        let meta = lower.lookup(path).map_err(fs_error)?;
        let mut data = alloc::vec![0u8; meta.size as usize];
        let read = lower.read(path, 0, &mut data).map_err(fs_error)?;
        data.truncate(read);
        Ok(data)
    }

    fn names(entries: &[vfs::DirEntry]) -> Vec<String> {
        entries.iter().map(|entry| entry.name.clone()).collect()
    }

    fn has(entries: &[vfs::DirEntry], name: &str) -> bool {
        entries.iter().any(|entry| entry.name == name)
    }

    /// Copy-up makes the first write land in the upper layer: reads fall
    /// through, read-your-writes holds, metadata sizes track, and the lower
    /// layer is byte-identical afterwards. Also covers `read_dir` union and
    /// the unlink/whiteout cycle for lower and upper entries.
    pub fn copy_up_read_write() -> Result<(), String> {
        let lower = lower_fixture()?;
        let (mut vfs, overlay) = mounted_overlay(lower.clone())?;
        let root = Id::ROOT;

        // Lower reads fall through unchanged.
        let meta = vfs.stat(root, "/HELLO.TXT").map_err(fs_error)?;
        check!(
            meta.kind == FileKind::File && meta.size == 17,
            "lower meta is {meta:?}"
        );
        check!(
            vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello from LazyOS",
            "lower read differs"
        );
        check!(
            overlay.usage() == (0, 1),
            "untouched overlay usage {:?}",
            overlay.usage()
        );

        // The first write copies the file up and changes only the upper copy.
        check!(
            vfs.write(root, "/HELLO.TXT", 6, b"aBI ")
                .map_err(fs_error)?
                == 4,
            "copy-up write was short"
        );
        check!(
            vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello aBI  LazyOS",
            "read-your-writes failed"
        );
        check!(
            vfs.stat(root, "/HELLO.TXT").map_err(fs_error)?.size == 17,
            "size after overwrite"
        );
        check!(
            lower_bytes(&lower, "HELLO.TXT")? == b"Hello from LazyOS",
            "copy-up modified the lower layer"
        );
        check!(
            overlay.usage().0 == 17,
            "copy-up usage {:?}",
            overlay.usage()
        );

        // A write past EOF zero-fills and grows the reported size.
        check!(
            vfs.write(root, "/HELLO.TXT", 20, b"!").map_err(fs_error)? == 1,
            "extending write was short"
        );
        check!(
            vfs.stat(root, "/HELLO.TXT").map_err(fs_error)?.size == 21,
            "extended size"
        );
        check!(
            vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello aBI  LazyOS\0\0\0!",
            "sparse extension bytes"
        );

        // Truncate shrinks the cached metadata and the data.
        vfs.truncate(root, "/HELLO.TXT", 5).map_err(fs_error)?;
        check!(
            vfs.stat(root, "/HELLO.TXT").map_err(fs_error)?.size == 5
                && vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello",
            "truncate did not shrink"
        );

        // A write deep in a lower-only tree copies the ancestor dirs up too.
        vfs.write(root, "/LDIR/INNER.TXT", 0, b"INNER")
            .map_err(fs_error)?;
        check!(
            vfs.read_file(root, "/LDIR/INNER.TXT").map_err(fs_error)? == b"INNER",
            "nested copy-up read"
        );
        check!(
            lower_bytes(&lower, "LDIR/INNER.TXT")? == b"inner",
            "nested copy-up modified the lower layer"
        );

        // create + write + read-back, then the union listing.
        vfs.create(root, "/NEW.TXT", 0o644).map_err(fs_error)?;
        vfs.write(root, "/NEW.TXT", 0, b"new").map_err(fs_error)?;
        check!(
            vfs.read_file(root, "/NEW.TXT").map_err(fs_error)? == b"new",
            "new file round-trip"
        );
        let listing = vfs.readdir(root, "/").map_err(fs_error)?;
        for expected in ["HELLO.TXT", "LOWER.TXT", "LDIR", "NEW.TXT"] {
            check!(
                has(&listing, expected),
                "readdir union missing {expected}: {:?}",
                names(&listing)
            );
        }

        // Unlinking an upper file removes it and leaves no whiteout behind.
        vfs.unlink(root, "/NEW.TXT").map_err(fs_error)?;
        check!(
            vfs.stat(root, "/NEW.TXT").err() == Some(FsError::NotFound),
            "unlinked upper file still resolves"
        );
        check!(
            !has(&vfs.readdir(root, "/").map_err(fs_error)?, "NEW.TXT"),
            "readdir shows it"
        );
        let before = overlay.usage();

        // Unlinking a lower-only file hides it without touching the lower.
        vfs.unlink(root, "/LOWER.TXT").map_err(fs_error)?;
        check!(
            vfs.stat(root, "/LOWER.TXT").err() == Some(FsError::NotFound),
            "whiteout did not hide the lower file"
        );
        check!(
            !has(&vfs.readdir(root, "/").map_err(fs_error)?, "LOWER.TXT"),
            "readdir still lists a whiteout"
        );
        check!(
            lower_bytes(&lower, "LOWER.TXT")? == b"lower",
            "whiteout modified the lower layer"
        );
        check!(
            overlay.usage().1 == before.1 + 1,
            "whiteout did not account a node: {:?} -> {:?}",
            before,
            overlay.usage()
        );

        // Re-creating the name clears the whiteout and shows the new bytes.
        vfs.create(root, "/LOWER.TXT", 0o644).map_err(fs_error)?;
        vfs.write(root, "/LOWER.TXT", 0, b"fresh")
            .map_err(fs_error)?;
        check!(
            vfs.read_file(root, "/LOWER.TXT").map_err(fs_error)? == b"fresh",
            "re-created file shows stale bytes"
        );
        check!(
            has(&vfs.readdir(root, "/").map_err(fs_error)?, "LOWER.TXT"),
            "re-created file missing from readdir"
        );
        Ok(())
    }

    /// Directory lifecycle: nested `mkdir`, `rmdir` emptiness rules, whiteouts
    /// for lower-only directories, and the errors the VFS classifies.
    pub fn dir_create_remove() -> Result<(), String> {
        let lower = lower_fixture()?;
        let (mut vfs, _) = mounted_overlay(lower.clone())?;
        let root = Id::ROOT;

        // The fixture's `create_dir_all("ABIDIR/SUB")` shape.
        vfs.mkdir(root, "/ABIDIR", 0o755).map_err(fs_error)?;
        vfs.mkdir(root, "/ABIDIR/SUB", 0o755).map_err(fs_error)?;
        let sub = vfs.readdir(root, "/ABIDIR").map_err(fs_error)?;
        check!(names(&sub) == ["SUB"], "ABIDIR is {:?}", names(&sub));
        check!(
            vfs.mkdir(root, "/ABIDIR", 0o755).err() == Some(FsError::Exists),
            "mkdir over an existing dir"
        );
        check!(
            vfs.rmdir(root, "/ABIDIR").err() == Some(FsError::NotEmpty),
            "rmdir removed a non-empty dir"
        );
        vfs.create(root, "/ABIDIR/SUB/F.TXT", 0o644)
            .map_err(fs_error)?;
        check!(
            vfs.unlink(root, "/ABIDIR/SUB").err() == Some(FsError::IsDir),
            "unlink removed a dir"
        );
        check!(
            vfs.rmdir(root, "/ABIDIR/SUB").err() == Some(FsError::NotEmpty),
            "rmdir on the child dir"
        );
        vfs.unlink(root, "/ABIDIR/SUB/F.TXT").map_err(fs_error)?;
        vfs.rmdir(root, "/ABIDIR/SUB").map_err(fs_error)?;
        vfs.rmdir(root, "/ABIDIR").map_err(fs_error)?;
        check!(
            vfs.stat(root, "/ABIDIR").err() == Some(FsError::NotFound),
            "removed dir still resolves"
        );

        // A lower-only directory is shadowed by a whiteout after its contents
        // are removed, and the lower layer keeps both entries.
        check!(
            vfs.rmdir(root, "/LDIR").err() == Some(FsError::NotEmpty),
            "rmdir of a lower dir with contents"
        );
        vfs.unlink(root, "/LDIR/INNER.TXT").map_err(fs_error)?;
        check!(
            vfs.rmdir(root, "/LDIR").map_err(fs_error).is_ok(),
            "rmdir of a lower dir after removing its contents"
        );
        check!(
            vfs.stat(root, "/LDIR").err() == Some(FsError::NotFound)
                && vfs.stat(root, "/LDIR/INNER.TXT").err() == Some(FsError::NotFound),
            "whiteouted lower dir still resolves"
        );
        check!(
            lower.lookup("LDIR").is_ok() && lower.lookup("LDIR/INNER.TXT").is_ok(),
            "whiteout modified the lower layer"
        );
        Ok(())
    }

    /// Rename semantics: move a lower file out and back, replace a lower file
    /// with an upper one, and the file/dir type and emptiness errors.
    pub fn rename_replace() -> Result<(), String> {
        let lower = lower_fixture()?;
        let (mut vfs, _) = mounted_overlay(lower.clone())?;
        let root = Id::ROOT;

        // The fixture's rename-out-and-back pattern.
        vfs.rename(root, "/HELLO.TXT", "/ABIREN.TXT")
            .map_err(fs_error)?;
        check!(
            vfs.stat(root, "/HELLO.TXT").err() == Some(FsError::NotFound),
            "rename left the lower source visible"
        );
        check!(
            vfs.read_file(root, "/ABIREN.TXT").map_err(fs_error)? == b"Hello from LazyOS",
            "rename lost the contents"
        );
        check!(
            !has(&vfs.readdir(root, "/").map_err(fs_error)?, "HELLO.TXT")
                && has(&vfs.readdir(root, "/").map_err(fs_error)?, "ABIREN.TXT"),
            "readdir disagrees with the rename"
        );
        vfs.rename(root, "/ABIREN.TXT", "/HELLO.TXT")
            .map_err(fs_error)?;
        check!(
            vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello from LazyOS",
            "rename back lost the contents"
        );
        check!(
            lower_bytes(&lower, "HELLO.TXT")? == b"Hello from LazyOS",
            "rename modified the lower layer"
        );

        // An upper file replaces a lower file at the destination.
        vfs.create(root, "/TMP.TXT", 0o644).map_err(fs_error)?;
        vfs.write(root, "/TMP.TXT", 0, b"replacement")
            .map_err(fs_error)?;
        vfs.rename(root, "/TMP.TXT", "/LOWER.TXT")
            .map_err(fs_error)?;
        check!(
            vfs.read_file(root, "/LOWER.TXT").map_err(fs_error)? == b"replacement",
            "replace rename kept stale bytes"
        );
        check!(
            lower_bytes(&lower, "LOWER.TXT")? == b"lower",
            "replace rename modified the lower layer"
        );
        check!(
            vfs.readdir(root, "/")
                .map_err(fs_error)?
                .iter()
                .filter(|entry| entry.name == "LOWER.TXT")
                .count()
                == 1,
            "replace rename duplicated the destination"
        );

        // Type and emptiness rules.
        vfs.mkdir(root, "/DIR", 0o755).map_err(fs_error)?;
        vfs.create(root, "/FILE.TXT", 0o644).map_err(fs_error)?;
        check!(
            vfs.rename(root, "/FILE.TXT", "/DIR").err() == Some(FsError::IsDir),
            "file replaced a directory"
        );
        check!(
            vfs.rename(root, "/DIR", "/FILE.TXT").err() == Some(FsError::NotDir),
            "directory replaced a file"
        );
        vfs.mkdir(root, "/DIR2", 0o755).map_err(fs_error)?;
        vfs.create(root, "/DIR2/X.TXT", 0o644).map_err(fs_error)?;
        check!(
            vfs.rename(root, "/DIR", "/DIR2").err() == Some(FsError::NotEmpty),
            "directory replaced a non-empty directory"
        );
        vfs.mkdir(root, "/EMPTY", 0o755).map_err(fs_error)?;
        vfs.rename(root, "/EMPTY", "/DIR").map_err(fs_error)?;
        check!(
            vfs.stat(root, "/DIR").is_ok()
                && vfs.stat(root, "/EMPTY").err() == Some(FsError::NotFound),
            "empty-dir replace failed"
        );
        check!(
            vfs.rename(root, "/DIR", "/DIR/SUB").err() == Some(FsError::Invalid),
            "rename moved a directory into itself"
        );
        Ok(())
    }

    /// The upper layer is capped: byte and node growth past the limit answers
    /// `NoSpace`, and removing everything returns usage to the baseline.
    pub fn enospc_limits() -> Result<(), String> {
        let lower = lower_fixture()?;
        let overlay = Arc::new(Overlay::with_limits(lower, 8, 4));
        let mut vfs = Vfs::new();
        vfs.mount("/", overlay.clone()).map_err(fs_error)?;
        let root = Id::ROOT;
        let baseline = overlay.usage();

        vfs.create(root, "/A.TXT", 0o644).map_err(fs_error)?;
        vfs.write(root, "/A.TXT", 0, b"12345678")
            .map_err(fs_error)?;
        check!(
            vfs.write(root, "/A.TXT", 8, b"9").err() == Some(FsError::NoSpace),
            "byte cap was not enforced"
        );
        check!(
            vfs.truncate(root, "/A.TXT", 9).err() == Some(FsError::NoSpace),
            "truncate cap was not enforced"
        );
        check!(
            vfs.read_file(root, "/A.TXT").map_err(fs_error)? == b"12345678",
            "a refused write changed the file"
        );

        // Node cap: root + A occupy two, B and C fill the four, D is refused.
        vfs.create(root, "/B.TXT", 0o644).map_err(fs_error)?;
        vfs.create(root, "/C.TXT", 0o644).map_err(fs_error)?;
        check!(
            vfs.create(root, "/D.TXT", 0o644).err() == Some(FsError::NoSpace),
            "node cap was not enforced"
        );

        // Cleanup returns to baseline exactly.
        vfs.unlink(root, "/A.TXT").map_err(fs_error)?;
        check!(
            overlay.usage().0 == 0,
            "unlink did not release the bytes: {:?}",
            overlay.usage()
        );
        vfs.unlink(root, "/B.TXT").map_err(fs_error)?;
        vfs.unlink(root, "/C.TXT").map_err(fs_error)?;
        check!(
            overlay.usage() == baseline,
            "upper scratch did not return to baseline: {:?}",
            overlay.usage()
        );
        Ok(())
    }

    /// Soak: many create/write/rename/unlink and mkdir/rmdir generations must
    /// leave no upper bytes or nodes behind, and lower-file rename churn must
    /// stay bounded (the copy-up persists exactly once).
    pub fn soak_generations() -> Result<(), String> {
        const GENERATIONS: usize = 128;
        let lower = lower_fixture()?;
        let (mut vfs, overlay) = mounted_overlay(lower.clone())?;
        let root = Id::ROOT;
        let baseline = overlay.usage();

        for generation in 0..GENERATIONS {
            let file = format!("/GEN{generation}.TXT");
            let moved = format!("/MOVED{generation}.TXT");
            let dir = format!("/GDIR{generation}");
            let inner = format!("/GDIR{generation}/INNER.TXT");

            vfs.create(root, &file, 0o644).map_err(fs_error)?;
            vfs.write(root, &file, 0, b"payload").map_err(fs_error)?;
            check!(
                vfs.stat(root, &file).map_err(fs_error)?.size == 7,
                "generation {generation}: size is stale"
            );
            vfs.rename(root, &file, &moved).map_err(fs_error)?;
            check!(
                vfs.read_file(root, &moved).map_err(fs_error)? == b"payload",
                "generation {generation}: renamed bytes differ"
            );
            vfs.unlink(root, &moved).map_err(fs_error)?;

            vfs.mkdir(root, &dir, 0o755).map_err(fs_error)?;
            vfs.create(root, &inner, 0o644).map_err(fs_error)?;
            vfs.write(root, &inner, 0, b"x").map_err(fs_error)?;
            vfs.unlink(root, &inner).map_err(fs_error)?;
            vfs.rmdir(root, &dir).map_err(fs_error)?;

            check!(
                overlay.usage() == baseline,
                "generation {generation} leaked: {:?} -> {:?}",
                baseline,
                overlay.usage()
            );
        }

        // Lower-file rename churn copies up once; usage must not grow per pass.
        for _ in 0..32 {
            vfs.rename(root, "/HELLO.TXT", "/ABIREN.TXT")
                .map_err(fs_error)?;
            vfs.rename(root, "/ABIREN.TXT", "/HELLO.TXT")
                .map_err(fs_error)?;
        }
        let churn = overlay.usage();
        check!(
            churn.1 <= baseline.1 + 2 && churn.0 <= baseline.0 + 17,
            "lower rename churn grew unbounded: {churn:?} from {baseline:?}"
        );
        check!(
            vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello from LazyOS",
            "rename churn lost the contents"
        );
        Ok(())
    }

    /// The Linux `*` syscalls reach the ABI overlay: mkdir/rename/unlink/rmdir,
    /// fd-relative `openat`/`unlinkat` (the `remove_dir_all` walk), and the
    /// native table staying read-only.
    pub fn abi_syscalls() -> Result<(), String> {
        const AT_FDCWD: u64 = (-100i64) as u64;
        const O_WRONLY: u64 = 1;
        const O_CREAT: u64 = 0o100;
        const O_EXCL: u64 = 0o200;
        const O_DIRECTORY: u64 = 0o200000;
        const AT_REMOVEDIR: u64 = 0x200;

        task::register_kernel();
        check!(crate::fs::init(), "the boot volume did not mount");

        let mounts = crate::fs::abi_mounts();
        check!(
            mounts
                .iter()
                .any(|(point, name)| point == "/" && *name == "overlay (abi rw)"),
            "ABI root is not the overlay: {mounts:?}"
        );
        check!(
            mounts
                .iter()
                .any(|(point, name)| point == "/tmp" && *name == "ramfs"),
            "ABI /tmp is not ramfs: {mounts:?}"
        );

        let dir = b"/ABIDIR\0";
        let sub = b"/ABIDIR/SUB\0";
        let from = b"/ABIDIR/SUB\0";
        let to = b"/ABIDIR/SUB2\0";
        let eexist = (-17i64) as u64;

        check!(
            process::linux::dispatch_for_test(83, dir.as_ptr() as u64, 0o755, 0) == 0,
            "mkdir failed"
        );
        check!(
            process::linux::dispatch_for_test(83, dir.as_ptr() as u64, 0o755, 0) == eexist,
            "mkdir over an existing directory did not answer EEXIST"
        );
        check!(
            process::linux::dispatch_for_test(258, AT_FDCWD, sub.as_ptr() as u64, 0o755) == 0,
            "mkdirat failed"
        );

        // fd-relative creation: open the directory, then create through it.
        let dirfd =
            process::linux::dispatch_for_test(257, AT_FDCWD, dir.as_ptr() as u64, O_DIRECTORY);
        check!(
            (3..task::FD_COUNT as u64).contains(&dirfd),
            "dirfd is {dirfd:#x}"
        );
        let child = b"CHILD.TXT\0";
        let childfd = process::linux::dispatch_for_test(
            257,
            dirfd,
            child.as_ptr() as u64,
            O_WRONLY | O_CREAT | O_EXCL,
        );
        check!(
            (3..task::FD_COUNT as u64).contains(&childfd),
            "fd-relative openat is {childfd:#x}"
        );
        check!(
            process::linux::dispatch_for_test(3, childfd, 0, 0) == 0,
            "close child failed"
        );
        check!(
            process::linux::dispatch_for_test(263, dirfd, child.as_ptr() as u64, 0) == 0,
            "fd-relative unlinkat failed"
        );
        check!(
            process::linux::dispatch_for_test(3, dirfd, 0, 0) == 0,
            "close dir failed"
        );

        // rename + rmdir through the plain syscalls.
        check!(
            process::linux::dispatch_for_test(82, from.as_ptr() as u64, to.as_ptr() as u64, 0) == 0,
            "rename failed"
        );
        check!(
            process::linux::dispatch_for_test(263, AT_FDCWD, to.as_ptr() as u64, AT_REMOVEDIR) == 0,
            "unlinkat(AT_REMOVEDIR) failed"
        );
        check!(
            process::linux::dispatch_for_test(84, dir.as_ptr() as u64, 0, 0) == 0,
            "rmdir failed"
        );

        // The overlay-visible path is gone, and the native table never saw it.
        check!(
            crate::fs::abi_stat(Id::ROOT, "/ABIDIR").err() == Some(FsError::NotFound),
            "ABIDIR still resolves through the ABI"
        );
        check!(
            crate::fs::vfs_stat(Id::ROOT, "/ABIDIR").err() == Some(FsError::NotFound),
            "ABIDIR leaked into the native table"
        );
        check!(
            crate::fs::vfs_create(Id::ROOT, "/NATIVE.TXT", 0o644).err() == Some(FsError::ReadOnly),
            "the native FAT root is no longer read-only"
        );
        Ok(())
    }

    /// `unlink` while a descriptor is open: the fd snapshot keeps reading, the
    /// path stops resolving, and a later write through the orphan answers
    /// ENOENT (the documented snapshot-model gap).
    pub fn unlink_while_open() -> Result<(), String> {
        const AT_FDCWD: u64 = (-100i64) as u64;
        const O_WRONLY: u64 = 1;
        const O_CREAT: u64 = 0o100;
        const O_TRUNC: u64 = 0o1000;
        let enoent = (-2i64) as u64;

        task::register_kernel();
        check!(crate::fs::init(), "the boot volume did not mount");
        let path = b"/ABIOPEN.TXT\0";
        let _ = crate::fs::abi_unlink(Id::ROOT, "/ABIOPEN.TXT");

        let fd = process::linux::dispatch_for_test(
            257,
            AT_FDCWD,
            path.as_ptr() as u64,
            O_WRONLY | O_CREAT | O_TRUNC,
        );
        check!(
            (3..task::FD_COUNT as u64).contains(&fd),
            "openat is {fd:#x}"
        );
        let payload = b"still readable";
        check!(
            process::linux::dispatch_for_test(1, fd, payload.as_ptr() as u64, payload.len() as u64)
                == payload.len() as u64,
            "write failed"
        );
        let mut stat = [0u8; 144];
        check!(
            process::linux::dispatch_for_test(5, fd, stat.as_mut_ptr() as u64, 0) == 0,
            "fstat failed"
        );
        check!(
            u64::from_le_bytes(stat[48..56].try_into().unwrap()) == payload.len() as u64,
            "fstat size is stale"
        );

        check!(
            process::linux::dispatch_for_test(87, path.as_ptr() as u64, 0, 0) == 0,
            "unlink failed"
        );
        check!(
            crate::fs::abi_stat(Id::ROOT, "/ABIOPEN.TXT").err() == Some(FsError::NotFound),
            "unlinked path still resolves"
        );

        // The open descriptor still reads its snapshot...
        check!(
            process::linux::dispatch_for_test(8, fd, 0, 0) == 0,
            "lseek failed"
        );
        let mut buf = [0u8; 32];
        let read =
            process::linux::dispatch_for_test(0, fd, buf.as_mut_ptr() as u64, buf.len() as u64);
        check!(
            read == payload.len() as u64 && &buf[..payload.len()] == payload,
            "the open fd lost its snapshot: read={read}"
        );
        // ...but a write through the orphan has no backing path.
        check!(
            process::linux::dispatch_for_test(1, fd, payload.as_ptr() as u64, 1) == enoent,
            "writing through an unlinked fd did not answer ENOENT"
        );
        check!(
            process::linux::dispatch_for_test(3, fd, 0, 0) == 0,
            "close failed"
        );
        Ok(())
    }
}
// ---------------------------------------------------------------------------
// ext2 read/write filesystem (issue #99)
// ---------------------------------------------------------------------------

mod ext2_suite {
    use super::block_suite::FakeDisk;
    use super::*;
    use crate::block::{self, SECTOR_SIZE};
    use crate::fs::ext2::Ext2;
    use crate::fs::vfs::{self, FileKind, FsError, Id, Vfs};
    use alloc::sync::Arc;
    use core::sync::atomic::Ordering;

    /// Sectors in every test disk (512 KiB at 512 bytes per sector).
    const DISK_SECTORS: usize = 1024;
    /// Byte offset of the ext2 superblock (fixed by the format).
    const SUPER: usize = 1024;

    fn put16(image: &mut [u8], offset: usize, value: u16) {
        image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Friendly, debuggable conversion for `?` in tests.
    fn fs_error(error: FsError) -> String {
        format!("{} ({error:?})", error.message())
    }

    /// A miniature `mke2fs` for the tests: one block group, 64 inodes, and a
    /// root directory holding only `.` and `..`. It lays out exactly the
    /// structures `Ext2::open` validates, so the suite exercises the real
    /// on-disk format with no disk image and no userspace tool. Returns the
    /// raw image for `total_blocks` blocks of the requested size.
    fn mkfs(block_size: u32, total_blocks: u32, inode_count: u32) -> Vec<u8> {
        let mut image = vec![0u8; DISK_SECTORS * SECTOR_SIZE];
        let bs = block_size as usize;
        let inode_size = 128usize;
        // Blocks 0..first_data hold the boot block; the superblock is at byte
        // 1024, i.e. block 1 for 1K blocks and block 0 for 2K/4K blocks.
        let first_data = if block_size == 1024 { 1 } else { 0 };
        let gdt = first_data + 1;
        let block_bitmap = gdt + 1;
        let inode_bitmap = gdt + 2;
        let inode_table = gdt + 3;
        let table_blocks = (inode_count as usize * inode_size).div_ceil(bs) as u32;
        let root_block = inode_table + table_blocks;
        let used_end = root_block + 1;
        let free_blocks = total_blocks - used_end;
        // Inodes 1..10 are reserved (the root is inode 2 among them); the
        // free count must not count them.
        let free_inodes = inode_count - 10;

        // Superblock.
        put32(&mut image, SUPER + 0x00, inode_count);
        put32(&mut image, SUPER + 0x04, total_blocks);
        put32(&mut image, SUPER + 0x0C, free_blocks);
        put32(&mut image, SUPER + 0x10, free_inodes);
        put32(&mut image, SUPER + 0x14, first_data);
        put32(&mut image, SUPER + 0x18, block_size.trailing_zeros() - 10);
        put32(&mut image, SUPER + 0x1C, block_size.trailing_zeros() - 10);
        put32(&mut image, SUPER + 0x20, block_size * 8);
        put32(&mut image, SUPER + 0x24, block_size * 8);
        put32(&mut image, SUPER + 0x28, inode_count);
        put16(&mut image, SUPER + 0x38, 0xEF53);
        put16(&mut image, SUPER + 0x3A, 1); // clean
        put16(&mut image, SUPER + 0x3C, 1); // continue on errors
        put32(&mut image, SUPER + 0x4C, 1); // revision 1
        put32(&mut image, SUPER + 0x54, 11); // first non-reserved inode
        put16(&mut image, SUPER + 0x58, inode_size as u16);
        put32(&mut image, SUPER + 0x60, 0x2); // incompat: filetype
        image[SUPER + 0x78..SUPER + 0x88].copy_from_slice(b"lazyos-ext2\0\0\0\0\0");

        // Group descriptor 0.
        let gd = gdt as usize * bs;
        put32(&mut image, gd + 0x00, block_bitmap);
        put32(&mut image, gd + 0x04, inode_bitmap);
        put32(&mut image, gd + 0x08, inode_table);
        put16(&mut image, gd + 0x0C, free_blocks as u16);
        put16(&mut image, gd + 0x0E, free_inodes as u16);
        put16(&mut image, gd + 0x10, 1); // the root is one directory

        // Block bitmap: every metadata block plus the root block is used; the
        // bits past the volume are padding (set, like mke2fs).
        let bitmap = block_bitmap as usize * bs;
        let bitmap_bits = block_size * 8; // blocks per group
        for bit in 0..bitmap_bits {
            let block = first_data + bit;
            if block < used_end || block >= total_blocks {
                image[bitmap + (bit / 8) as usize] |= 1 << (bit % 8);
            }
        }
        // Inode bitmap: the reserved inodes 1..10 (including the root) are
        // used, and the bits past `inode_count` are padding.
        let ib = inode_bitmap as usize * bs;
        for ino in 1..11.min(inode_count + 1) {
            image[ib + ((ino - 1) / 8) as usize] |= 1 << ((ino - 1) % 8);
        }
        for bit in inode_count..(block_size * 8) {
            image[ib + (bit / 8) as usize] |= 1 << (bit % 8);
        }

        // Root inode (number 2). `i_dtime` is a full 32-bit field, so gid,
        // links, and i_blocks start at 0x18, 0x1A, and 0x1C.
        let root_inode = inode_table as usize * bs + inode_size;
        put16(&mut image, root_inode + 0x00, 0o040755);
        put32(&mut image, root_inode + 0x04, block_size); // size
        put32(&mut image, root_inode + 0x08, 1); // atime
        put32(&mut image, root_inode + 0x0C, 1); // ctime
        put32(&mut image, root_inode + 0x10, 1); // mtime
        put16(&mut image, root_inode + 0x1A, 2); // links
        put32(&mut image, root_inode + 0x1C, block_size / 512); // i_blocks
        put32(&mut image, root_inode + 0x28, root_block);

        // Root directory block: `.` then `..` filling the block.
        let root = root_block as usize * bs;
        put32(&mut image, root, 2);
        put16(&mut image, root + 4, 12);
        image[root + 6] = 1;
        image[root + 7] = 2;
        image[root + 8] = b'.';
        put32(&mut image, root + 12, 2);
        put16(&mut image, root + 16, (bs - 12) as u16);
        image[root + 18] = 2;
        image[root + 19] = 2;
        image[root + 20] = b'.';
        image[root + 21] = b'.';

        image
    }

    /// Format a fresh fake disk, open it, and mount it as a private VFS root.
    /// The disk comes back too, so tests can watch its write/flush counters.
    fn mounted(
        block_size: u32,
        total_blocks: u32,
    ) -> Result<(Arc<Ext2>, Vfs, &'static FakeDisk), String> {
        let image = mkfs(block_size, total_blocks, 64);
        let disk = FakeDisk::new("test-ext2", DISK_SECTORS);
        disk.data.lock().copy_from_slice(&image);
        let fs = Arc::new(Ext2::open(disk).map_err(fs_error)?);
        let mut vfs = Vfs::new();
        vfs.mount("/", fs.clone()).map_err(fs_error)?;
        Ok((fs, vfs, disk))
    }

    /// Format, mount through the VFS, then run the whole op set: create,
    /// write, read (offset, direct, and single-indirect), stat, mkdir,
    /// readdir, rename (file and directory), unlink, sparse writes, and block
    /// reuse after unlink.
    pub fn create_write_read_rename_unlink() -> Result<(), String> {
        task::register_kernel();
        let (fs, mut vfs, disk) = mounted(1024, 512)?;
        let root = Id::ROOT;

        let meta = vfs.stat(root, "/").map_err(fs_error)?;
        check!(
            meta.kind == FileKind::Dir && meta.mode & vfs::S_IFMT == vfs::S_IFDIR,
            "root meta is {meta:?}"
        );

        // A directory and a small file living in direct blocks.
        vfs.mkdir(root, "/docs", 0o755).map_err(fs_error)?;
        vfs.create(root, "/docs/note.txt", 0o644)
            .map_err(fs_error)?;
        let meta = vfs.stat(root, "/docs/note.txt").map_err(fs_error)?;
        check!(
            meta.kind == FileKind::File
                && meta.size == 0
                && meta.mode & vfs::S_IFMT == vfs::S_IFREG,
            "file meta is {meta:?}"
        );
        check!(
            vfs.write(root, "/docs/note.txt", 0, b"hello")
                .map_err(fs_error)?
                == 5,
            "the first write was short"
        );
        vfs.write(root, "/docs/note.txt", 5, b" world")
            .map_err(fs_error)?;
        check!(
            vfs.read_file(root, "/docs/note.txt").map_err(fs_error)? == b"hello world".to_vec(),
            "the file contents are wrong"
        );
        let mut buf = [0u8; 4];
        let read = vfs
            .read(root, "/docs/note.txt", 6, &mut buf)
            .map_err(fs_error)?;
        check!(
            read == 4 && &buf == b"worl",
            "offset read got {read} {buf:?}"
        );
        check!(
            vfs.read(root, "/docs/note.txt", 99, &mut buf)
                .map_err(fs_error)?
                == 0,
            "a read past EOF did not return 0"
        );
        check!(
            vfs.readdir(root, "/docs").map_err(fs_error)?.len() == 1,
            "readdir did not see exactly note.txt"
        );

        // A file past the twelve direct slots: the single indirect block must
        // appear, and an overwrite across the boundary must land correctly.
        let mut big = vec![0u8; 40 * 1024];
        for (index, byte) in big.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        vfs.create(root, "/big.bin", 0o644).map_err(fs_error)?;
        vfs.write(root, "/big.bin", 0, &big).map_err(fs_error)?;
        check!(
            fs.mapped_block("/big.bin", 12).map_err(fs_error)? != 0,
            "the single indirect block was not allocated"
        );
        check!(
            vfs.read_file(root, "/big.bin").map_err(fs_error)? == big,
            "the 40 KiB round trip differs"
        );
        let patch = 12 * 1024 - 8;
        vfs.write(root, "/big.bin", patch as u64, &[0xAB; 32])
            .map_err(fs_error)?;
        big[patch..patch + 32].fill(0xAB);
        check!(
            vfs.read_file(root, "/big.bin").map_err(fs_error)? == big,
            "the overwrite across the indirect boundary differs"
        );

        // A sparse write leaves a hole that reads back as zeros.
        vfs.create(root, "/sparse", 0o644).map_err(fs_error)?;
        vfs.write(root, "/sparse", 5000, b"tail")
            .map_err(fs_error)?;
        check!(
            fs.mapped_block("/sparse", 0).map_err(fs_error)? == 0,
            "the sparse write allocated its hole block"
        );
        let sparse = vfs.read_file(root, "/sparse").map_err(fs_error)?;
        check!(
            sparse.len() == 5004
                && sparse[..5000].iter().all(|&byte| byte == 0)
                && &sparse[5000..] == b"tail",
            "the sparse read is wrong"
        );

        // Rename keeps contents; directories move with their subtrees.
        vfs.rename(root, "/docs/note.txt", "/docs/memo.txt")
            .map_err(fs_error)?;
        check!(
            vfs.stat(root, "/docs/note.txt").err() == Some(FsError::NotFound),
            "rename left the source behind"
        );
        check!(
            vfs.read_file(root, "/docs/memo.txt").map_err(fs_error)? == b"hello world".to_vec(),
            "rename lost the contents"
        );
        vfs.mkdir(root, "/docs/sub", 0o755).map_err(fs_error)?;
        vfs.create(root, "/docs/sub/inner", 0o644)
            .map_err(fs_error)?;
        let names: Vec<String> = vfs
            .readdir(root, "/docs")
            .map_err(fs_error)?
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        check!(names == ["memo.txt", "sub"], "readdir is {names:?}");
        vfs.rename(root, "/docs/sub", "/docs/moved")
            .map_err(fs_error)?;
        check!(
            vfs.stat(root, "/docs/sub/inner").err() == Some(FsError::NotFound),
            "the moved directory still resolves at the old path"
        );
        check!(
            vfs.stat(root, "/docs/moved/inner").is_ok(),
            "the moved directory's child is missing"
        );
        vfs.unlink(root, "/docs/moved/inner").map_err(fs_error)?;
        vfs.unlink(root, "/docs/memo.txt").map_err(fs_error)?;
        check!(
            vfs.readdir(root, "/docs").map_err(fs_error)?.len() == 1,
            "unlink left entries behind"
        );

        // Freeing a file returns its blocks to the bitmap, and the next file
        // reuses them (first-fit allocation).
        let baseline = fs.free_blocks().map_err(fs_error)?;
        let baseline_inodes = fs.free_inodes().map_err(fs_error)?;
        vfs.create(root, "/reuse.bin", 0o644).map_err(fs_error)?;
        vfs.write(root, "/reuse.bin", 0, &[0x11; 8 * 1024])
            .map_err(fs_error)?;
        let first = fs.mapped_block("/reuse.bin", 0).map_err(fs_error)?;
        let after = fs.free_blocks().map_err(fs_error)?;
        check!(
            after == baseline - 8,
            "reuse.bin took {} blocks (baseline {baseline}, after {after}, first {first})",
            baseline - after
        );
        check!(
            fs.free_inodes().map_err(fs_error)? == baseline_inodes - 1,
            "reuse.bin did not take an inode"
        );
        vfs.unlink(root, "/reuse.bin").map_err(fs_error)?;
        check!(
            fs.free_blocks().map_err(fs_error)? == baseline,
            "unlink did not return the blocks"
        );
        check!(
            fs.free_inodes().map_err(fs_error)? == baseline_inodes,
            "unlink did not return the inode"
        );
        vfs.create(root, "/reuse2.bin", 0o644).map_err(fs_error)?;
        vfs.write(root, "/reuse2.bin", 0, &[0x22; 8 * 1024])
            .map_err(fs_error)?;
        check!(
            fs.mapped_block("/reuse2.bin", 0).map_err(fs_error)? == first,
            "the freed block was not reused"
        );
        check!(
            fs.free_blocks().map_err(fs_error)? == baseline - 8,
            "reuse accounting is off"
        );
        vfs.unlink(root, "/reuse2.bin").map_err(fs_error)?;
        check!(
            fs.free_blocks().map_err(fs_error)? == baseline,
            "the final free count is wrong"
        );

        // Error paths.
        check!(
            vfs.create(root, "/docs", 0o644).err() == Some(FsError::Exists),
            "create replaced a directory"
        );
        check!(
            vfs.write(root, "/docs", 0, b"x").err() == Some(FsError::IsDir),
            "write succeeded on a directory"
        );
        check!(
            vfs.unlink(root, "/docs").err() == Some(FsError::IsDir),
            "unlink removed a directory"
        );
        check!(
            vfs.rename(root, "/nope", "/docs/x").err() == Some(FsError::NotFound),
            "rename found a ghost"
        );
        check!(
            vfs.create(root, "/missing/file", 0o644).err() == Some(FsError::NotFound),
            "create succeeded in a missing directory"
        );
        let long = "x".repeat(256);
        check!(
            vfs.create(root, &format!("/{long}"), 0o644).err() == Some(FsError::NameTooLong),
            "an over-long name was accepted"
        );

        // flush reaches the device and stamps the superblock.
        let before = disk.flushes.load(Ordering::Relaxed);
        fs.flush().map_err(fs_error)?;
        check!(
            disk.flushes.load(Ordering::Relaxed) == before + 1,
            "flush did not reach the block device"
        );
        Ok(())
    }

    /// 1K, 2K, and 4K blocks all mount and round-trip a file that spills into
    /// the single-indirect map, so every block-size-dependent shift is used.
    pub fn block_sizes() -> Result<(), String> {
        task::register_kernel();
        for block_size in [1024u32, 2048, 4096] {
            let total_blocks = (DISK_SECTORS * SECTOR_SIZE) as u32 / block_size;
            let (fs, mut vfs, _disk) = mounted(block_size, total_blocks)?;
            check!(
                fs.block_size() == block_size,
                "open reported {} for a {block_size}-byte block",
                fs.block_size()
            );
            let root = Id::ROOT;
            vfs.mkdir(root, "/dir", 0o755).map_err(fs_error)?;
            vfs.create(root, "/dir/file", 0o644).map_err(fs_error)?;
            let payload: Vec<u8> = (0..60 * 1024).map(|index| (index % 253) as u8).collect();
            vfs.write(root, "/dir/file", 0, &payload)
                .map_err(fs_error)?;
            check!(
                fs.mapped_block("/dir/file", 12).map_err(fs_error)? != 0,
                "{block_size}: the indirect block is missing"
            );
            check!(
                vfs.read_file(root, "/dir/file").map_err(fs_error)? == payload,
                "{block_size}: the round trip differs"
            );
            // Rewriting the tail in place (no new allocation) must preserve
            // the leading blocks.
            let tail = payload.len() - 5;
            vfs.write(root, "/dir/file", tail as u64, b"12345")
                .map_err(fs_error)?;
            let mut expected = payload.clone();
            expected[tail..].copy_from_slice(b"12345");
            check!(
                vfs.read_file(root, "/dir/file").map_err(fs_error)? == expected,
                "{block_size}: the in-place rewrite differs"
            );
            vfs.unlink(root, "/dir/file").map_err(fs_error)?;
        }
        Ok(())
    }

    /// Malformed images answer friendly errors instead of panicking: bad
    /// magic, unsupported geometry, impossible counts, unsupported features,
    /// a truncated device, out-of-range group pointers, and a corrupt
    /// directory record.
    pub fn rejects_corruption() -> Result<(), String> {
        task::register_kernel();
        let good = mkfs(1024, 512, 64);
        let disk = FakeDisk::new("test-ext2-bad", DISK_SECTORS);

        // Swap an image into the shared disk and try to open it.
        let open_err = |image: &[u8]| -> Option<FsError> {
            disk.data.lock().copy_from_slice(image);
            Ext2::open(disk).err()
        };

        let mut bad = good.clone();
        bad[SUPER + 0x38] ^= 0xFF; // wrong magic
        check!(
            open_err(&bad) == Some(FsError::Invalid),
            "bad magic accepted"
        );

        let mut bad = good.clone();
        put32(&mut bad, SUPER + 0x18, 5); // log block size -> 32 KiB
        check!(
            open_err(&bad) == Some(FsError::Invalid),
            "an over-large block size was accepted"
        );

        let mut bad = good.clone();
        put16(&mut bad, SUPER + 0x58, 0); // zero inode size
        check!(
            open_err(&bad) == Some(FsError::Invalid),
            "a zero inode size was accepted"
        );

        let mut bad = good.clone();
        put32(&mut bad, SUPER + 0x60, 0x80); // incompat 64BIT
        check!(
            open_err(&bad) == Some(FsError::NotSupported),
            "the 64BIT feature was accepted"
        );

        let mut bad = good.clone();
        put32(&mut bad, SUPER + 0x64, 0x10); // ro-compat GDT_CSUM
        check!(
            open_err(&bad) == Some(FsError::NotSupported),
            "an unknown ro-compat feature was accepted"
        );

        let mut bad = good.clone();
        put32(&mut bad, SUPER + 0x04, 0xFFFF_FFFF); // block count past the device
        check!(
            open_err(&bad) == Some(FsError::Invalid),
            "an impossible block count was accepted"
        );

        let mut bad = good.clone();
        put32(&mut bad, SUPER + 0x10, 1000); // more free inodes than inodes
        check!(
            open_err(&bad) == Some(FsError::Invalid),
            "free inodes above the total were accepted"
        );

        // A device too short to hold even the superblock.
        let tiny = FakeDisk::new("test-ext2-tiny", 1);
        check!(
            Ext2::open(tiny).err() == Some(FsError::Invalid),
            "a one-sector device was accepted"
        );

        // A valid superblock hiding a group descriptor whose bitmap pointer
        // lies outside the volume: open succeeds, the first read refuses.
        let mut bad = good.clone();
        put32(&mut bad, 2 * 1024, 0xFFFF_FFFF); // group 0 block bitmap pointer
        disk.data.lock().copy_from_slice(&bad);
        let fs = Arc::new(Ext2::open(disk).map_err(fs_error)?);
        let mut vfs = Vfs::new();
        vfs.mount("/", fs).map_err(fs_error)?;
        check!(
            vfs.stat(Id::ROOT, "/").err() == Some(FsError::Invalid),
            "an out-of-range group pointer was accepted"
        );

        // A valid superblock hiding a corrupt directory record: `readdir`
        // must answer Invalid rather than loop or panic.
        let mut bad = good.clone();
        // For 1K blocks and the mkfs layout, root data block 13 is in the
        // 512-block image (gdt 2, bitmaps 3/4, inode table 5..12, root 13).
        put16(&mut bad, 13 * 1024 + 4, 3); // record length not a multiple of 4
        disk.data.lock().copy_from_slice(&bad);
        let fs = Arc::new(Ext2::open(disk).map_err(fs_error)?);
        let mut vfs = Vfs::new();
        vfs.mount("/", fs).map_err(fs_error)?;
        check!(
            vfs.readdir(Id::ROOT, "/").err() == Some(FsError::Invalid),
            "a corrupt directory record was accepted"
        );
        Ok(())
    }

    /// The `mount <dev>` surface opens ext2 on any registered device (not
    /// just the boot volume), and the mount is reachable through the global
    /// VFS helpers.
    pub fn mount_device_wiring() -> Result<(), String> {
        task::register_kernel();
        crate::fs::init();
        let image = mkfs(1024, 512, 64);
        let disk = FakeDisk::new("test-ext2-mount", DISK_SECTORS);
        disk.data.lock().copy_from_slice(&image);
        check!(
            block::register(disk).is_ok(),
            "registering the ext2 disk failed"
        );
        check!(
            crate::fs::mount_device("/ext2", "test-ext2-mount").is_ok(),
            "mount_device refused an ext2 volume"
        );
        check!(
            crate::fs::mount_device("/ext2", "test-ext2-mount").err() == Some(FsError::Exists),
            "a duplicate mount point was accepted"
        );

        let root = Id::ROOT;
        check!(
            crate::fs::vfs_stat(root, "/ext2").map_err(fs_error)?.kind == FileKind::Dir,
            "/ext2 is not a directory"
        );
        crate::fs::vfs_mkdir(root, "/ext2/home", 0o755).map_err(fs_error)?;
        crate::fs::vfs_create(root, "/ext2/home/file.txt", 0o644).map_err(fs_error)?;
        crate::fs::vfs_write(root, "/ext2/home/file.txt", 0, b"mounted ext2").map_err(fs_error)?;
        let data = crate::fs::vfs_read(root, "/ext2/home/file.txt").map_err(fs_error)?;
        check!(
            data == b"mounted ext2".to_vec(),
            "the global VFS read returned {data:?}"
        );
        crate::fs::vfs_unlink(root, "/ext2/home/file.txt").map_err(fs_error)?;
        Ok(())
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
// ---------------------------------------------------------------------------
// Service supervision primitives (issue #93)
// ---------------------------------------------------------------------------

/// The supervisor's kernel surface: `task::spawn_child` sets the parent link
/// (so a service is reapable) and the native `wait` syscall packs the exit
/// status into one register. `init` builds its restart loop on exactly this.
mod service_suite {
    use super::*;

    /// A minimal but valid static ELF64: one `PT_LOAD` segment with a single
    /// `hlt` byte. The loader maps it; the test never runs it.
    pub fn minimal_elf() -> Vec<u8> {
        let mut elf = vec![0u8; 0x120];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2; // ELFCLASS64
        elf[5] = 1; // little-endian
        elf[6] = 1; // version
        elf[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        elf[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
        elf[20..24].copy_from_slice(&1u32.to_le_bytes()); // version
        elf[24..32].copy_from_slice(&0x40_0000u64.to_le_bytes()); // entry
        elf[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        elf[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
        elf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        elf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        let ph = 64;
        elf[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        elf[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes()); // R|X
        elf[ph + 8..ph + 16].copy_from_slice(&0x100u64.to_le_bytes()); // p_offset
        elf[ph + 16..ph + 24].copy_from_slice(&0x40_0000u64.to_le_bytes()); // p_vaddr
        elf[ph + 24..ph + 32].copy_from_slice(&0x40_0000u64.to_le_bytes()); // p_paddr
        elf[ph + 32..ph + 40].copy_from_slice(&1u64.to_le_bytes()); // p_filesz
        elf[ph + 40..ph + 48].copy_from_slice(&1u64.to_le_bytes()); // p_memsz
        elf[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align
        elf[0x100] = 0xf4; // hlt
        elf
    }

    fn fresh() {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
    }

    /// `spawn_child` links the new task to the caller; after it exits, the
    /// native `wait` syscall reaps it and returns `(pid << 32) | status`.
    pub fn spawn_child_parent_and_wait() -> Result<(), String> {
        fresh();
        let elf = minimal_elf();
        let slot = task::spawn_child("svc", &elf).map_err(to_string)?;
        check!(
            (1..task::MAX_TASKS).contains(&slot),
            "spawn_child returned slot {slot}"
        );
        check!(
            task::process::ppid_of(slot) == task::KERNEL_TASK,
            "child ppid is {} (expected the kernel slot)",
            task::process::ppid_of(slot)
        );
        check!(task::reap_child().is_none(), "a running child was reapable");
        task::harness::finish(slot, 7);
        let packed = process::dispatch_for_test(7, 0, 0, 0);
        check!(
            packed != u64::MAX,
            "wait reported a timeout for a dead child"
        );
        check!(
            packed >> 32 == slot as u64,
            "wait returned pid {} (expected {slot})",
            packed >> 32
        );
        check!(
            packed & 0xffff_ffff == 7,
            "wait returned status {} (expected 7)",
            packed & 0xffff_ffff
        );
        check!(task::reap_child().is_none(), "the child was not reaped");
        Ok(())
    }

    /// A malformed image is refused without leaving a task slot or frames
    /// behind, so a supervisor retry cannot leak.
    pub fn spawn_child_rejects_bad_image() -> Result<(), String> {
        fresh();
        let result = task::spawn_child("bad", b"not an ELF image");
        check!(result.is_err(), "a malformed ELF was accepted");
        check!(
            task::snapshot(1).is_none(),
            "the failed spawn left a task in slot 1"
        );
        Ok(())
    }

    /// The native `spawn` syscall refuses a missing FAT entry with `u64::MAX`
    /// (the test harness boots before `fs::init`, so every name is missing).
    pub fn spawn_unknown_file_fails() -> Result<(), String> {
        fresh();
        static MISSING: &[u8] = b"NOSUCH.ELF\0";
        let packed = process::dispatch_for_test(6, MISSING.as_ptr() as u64, 0, 0);
        check!(
            packed == u64::MAX,
            "spawn of a missing file returned {packed:#x}"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Display device grant (issue #113)
// ---------------------------------------------------------------------------

/// The kernel side of the userspace compositor: syscall 12 grants the screen to
/// a task, queues input for it, and presents damage rectangles. These tests
/// drive the same entry point the `int 0x80` gate uses (`dispatch_for_test`) on
/// a scratch user task, so the whole path runs without a scheduler.
mod display_suite {
    use super::*;
    use crate::input::keyboard::Key;

    /// Two's-complement `-errno`, the syscall error encoding.
    fn failed(code: i64) -> u64 {
        (code as u64).wrapping_neg()
    }

    /// Slot of a scratch user task with its own handle table; `current()` is
    /// pointed at it so the grant's `task::current()` checks see a user.
    fn scratch_task() -> Result<usize, String> {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        let slot = task::spawn_fork().map_err(to_string)?;
        task::harness::switch_current(slot);
        Ok(slot)
    }

    /// The kernel multiplexer itself may not bind: the grant is for a user
    /// compositor, and the mux is the fallback owner.
    pub fn kernel_bind_refused() -> Result<(), String> {
        crate::display::reset();
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
        let mut info = [0u64; crate::display::INFO_WORDS];
        let code =
            process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
        check!(
            code == failed(1),
            "kernel bind -> {code:#x} (expected -EPERM)"
        );
        check!(!crate::display::bound(), "bound() after a refused bind");
        crate::display::reset();
        Ok(())
    }

    /// Bind returns the screen geometry and a mapped buffer; a second task is
    /// refused while the grant is live; input events round-trip; present
    /// reaches the real framebuffer; unbind releases the grant.
    pub fn bind_input_present_roundtrip() -> Result<(), String> {
        crate::display::reset();
        let first = scratch_task()?;

        // Bind: the screen buffer is created and mapped in this task.
        let mut info = [0u64; crate::display::INFO_WORDS];
        let code =
            process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
        check!(code == 0, "bind -> {code:#x}");
        let (width, height, va, size) = (info[0], info[1], info[5], info[6]);
        check!(
            width > 0 && height > 0 && width < 8192 && height < 8192,
            "implausible screen {width}x{height}"
        );
        check!(size == width * height * 4, "screen size {size}");
        check!(va != 0, "bind returned no buffer mapping");
        check!(crate::display::bound(), "bound() false after a live bind");

        // A second live task cannot steal the display.
        let second = task::spawn_fork().map_err(to_string)?;
        task::harness::switch_current(second);
        let mut other = [0u64; crate::display::INFO_WORDS];
        let code =
            process::dispatch_for_test(12, crate::display::op::BIND, other.as_mut_ptr() as u64, 0);
        check!(
            code == failed(16),
            "second bind -> {code:#x} (expected -EBUSY)"
        );
        task::harness::switch_current(first);

        // Write a red square into the screen buffer and present it: the pixel
        // must land in the real framebuffer.
        let square = (12usize, 20usize);
        for row in 0..2usize {
            for col in 0..2usize {
                let at = ((square.1 + row) * width as usize + square.0 + col) * 4;
                // Safety: inside the screen buffer `va` of `size` bytes.
                unsafe {
                    let pixel = (va as *mut u8).add(at);
                    pixel.write(0xF0);
                    pixel.add(1).write(0x10);
                    pixel.add(2).write(0x10);
                    pixel.add(3).write(0xFF);
                }
            }
        }
        let packed = square.0 as u64 | ((square.1 as u64) << 16) | (2 << 32) | (2 << 48);
        let code = process::dispatch_for_test(12, crate::display::op::PRESENT, packed, 0);
        check!(code == 0, "present -> {code:#x}");
        let color = crate::console::with_framebuffer(|fb| fb.read_pixel(square.0, square.1))
            .ok_or("no framebuffer")?;
        check!(
            color.r > 200 && color.g < 60 && color.b < 60,
            "presented pixel is {color:?}, expected red"
        );

        // Input queued for the owner comes back through the poll op. `bind`
        // seeds one pointer-move event, so drain that first.
        let mut drain = [0u8; 16];
        let seeded = process::dispatch_for_test(
            12,
            crate::display::op::INPUT_POLL,
            drain.as_mut_ptr() as u64,
            drain.len() as u64,
        );
        check!(seeded == 1, "bind seeded {seeded} events, expected 1");
        crate::display::push_pointer_move(100, 120);
        crate::display::push_key(Key::Char('a'), true);
        let mut events = [0u8; 32];
        let count = process::dispatch_for_test(
            12,
            crate::display::op::INPUT_POLL,
            events.as_mut_ptr() as u64,
            events.len() as u64,
        );
        check!(count == 2, "input_poll returned {count}, expected 2");
        check!(
            u32::from_le_bytes(events[0..4].try_into().unwrap()) == 0
                && i32::from_le_bytes(events[4..8].try_into().unwrap()) == 100,
            "first event is not a pointer move to (100, 120)"
        );
        check!(
            u32::from_le_bytes(events[16..20].try_into().unwrap()) == 3
                && i32::from_le_bytes(events[20..24].try_into().unwrap()) == 'a' as i32,
            "second event is not a key down for 'a'"
        );

        // Unbind releases the grant and the mux fallback sees it.
        let code = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
        check!(code == 0, "unbind -> {code:#x}");
        check!(!crate::display::bound(), "bound() after unbind");
        task::harness::switch_current(task::KERNEL_TASK);
        crate::display::reset();
        task::harness::reset();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// System statistics snapshot (issue #144)
// ---------------------------------------------------------------------------

/// The fixed-layout read-only snapshot behind syscall 14: version/layout
/// stability, the strict buffer contract, live counters and the task table.
/// The soak drives snapshots through repeated fork/exit/reclaim generations,
/// where a per-snapshot allocation or a translation leak would show as frame,
/// slab or task-row growth.
mod sysinfo_suite {
    use super::*;
    use crate::sysinfo;

    /// Two's-complement `-errno`, the syscall error encoding.
    fn failed(code: i64) -> u64 {
        (code as u64).wrapping_neg()
    }

    /// Register the kernel task and empty the table, as a normal boot starts.
    fn fresh() {
        task::register_kernel();
        task::harness::reset();
        task::harness::switch_current(task::KERNEL_TASK);
    }

    /// Scratch user buffer for the snapshot. The syscall validates its
    /// destination against the active CR3 as mapped, writable user memory, so
    /// each call installs a fresh address space with this range mapped.
    const SPACE: u64 = 0x0040_0000;
    const SPACE_PAGES: u64 = (sysinfo::SIZE + 4095) / 4096;

    /// Run `f` with [`SPACE`] mapped into a fresh user address space.
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

    /// A full snapshot through the syscall entry, decoded as raw words.
    fn snapshot() -> Result<[u64; sysinfo::WORDS], String> {
        in_space(|| {
            let code =
                process::dispatch_for_test(14, sysinfo::op::SNAPSHOT, SPACE, sysinfo::SIZE);
            check!(code == sysinfo::SIZE, "snapshot -> {code:#x}");
            let mut words = [0u64; sysinfo::WORDS];
            for (index, word) in words.iter_mut().enumerate() {
                // Safety: the scratch pages are mapped readable while installed.
                *word = unsafe { core::ptr::read_volatile((SPACE as *const u64).add(index)) };
            }
            Ok(words)
        })
    }

    /// The size op reports the ABI block; a null or short buffer and an
    /// unknown op are refused with the documented errno; a full buffer gets
    /// exactly one versioned block.
    pub fn snapshot_abi_contract() -> Result<(), String> {
        fresh();
        let size = process::dispatch_for_test(14, sysinfo::op::SIZE, 0, 0);
        check!(
            size == sysinfo::SIZE,
            "size op reported {size:#x}, expected {:#x}",
            sysinfo::SIZE
        );
        check!(
            process::dispatch_for_test(14, sysinfo::op::SNAPSHOT, 0, 0) == failed(14),
            "a null snapshot buffer was not refused with -EFAULT"
        );
        let short = in_space(|| {
            Ok(process::dispatch_for_test(
                14,
                sysinfo::op::SNAPSHOT,
                SPACE,
                sysinfo::SIZE - 8,
            ))
        })?;
        check!(
            short == failed(7),
            "a short buffer returned {short:#x}, expected -E2BIG"
        );
        check!(
            process::dispatch_for_test(14, 99, 0, 0) == failed(22),
            "an unknown op was not refused with -EINVAL"
        );

        let words = snapshot()?;
        check!(
            words[sysinfo::H_VERSION] == sysinfo::SYSTEM_STATS_VERSION,
            "header version is {}, expected {}",
            words[sysinfo::H_VERSION],
            sysinfo::SYSTEM_STATS_VERSION
        );
        check!(
            words[sysinfo::H_WORDS] == sysinfo::WORDS as u64,
            "header word count is {}, expected {}",
            words[sysinfo::H_WORDS],
            sysinfo::WORDS
        );
        check!(
            words[sysinfo::H_TASK_ROW_WORDS] == sysinfo::TASK_ROW_WORDS as u64
                && words[sysinfo::H_TASK_SLOTS] == task::MAX_TASKS as u64,
            "row layout words are {} rows {} slots, expected {} and {}",
            words[sysinfo::H_TASK_ROW_WORDS],
            words[sysinfo::H_TASK_SLOTS],
            sysinfo::TASK_ROW_WORDS,
            task::MAX_TASKS
        );
        Ok(())
    }

    /// Both read-only monitor gates (13: task list, 14: system stats) are open
    /// to every task, so a destination that is a kernel address, unmapped, or
    /// runs off the end of the mapping must be refused with `-EFAULT` before a
    /// byte is written — never turned into a kernel write.
    pub fn snapshot_rejects_bad_destinations() -> Result<(), String> {
        fresh();
        let mut canary = [0x5a5a_5a5a_5a5a_5a5au64; sysinfo::WORDS];
        let kernel_buf = canary.as_mut_ptr() as u64;
        in_space(|| {
            let tail = SPACE + SPACE_PAGES * 4096 - 8;
            for (label, buf) in [
                ("kernel address", kernel_buf),
                ("unmapped", 0xdead_0000),
                ("non-canonical", 0x0000_8000_0000_0000),
                ("range past the mapping", tail),
                ("wrapping range", u64::MAX - 8),
            ] {
                let code =
                    process::dispatch_for_test(14, sysinfo::op::SNAPSHOT, buf, sysinfo::SIZE);
                check!(code == failed(14), "sysinfo {label} -> {code:#x}, expected -EFAULT");
                let code = process::dispatch_for_test(13, buf, 0, 0);
                check!(code == failed(14), "sys_tasks {label} -> {code:#x}, expected -EFAULT");
            }
            let code = process::dispatch_for_test(13, SPACE, 0, 0);
            check!(code == 0, "sys_tasks into a mapped buffer -> {code:#x}");
            Ok(())
        })?;
        check!(
            canary.iter().all(|&word| word == 0x5a5a_5a5a_5a5a_5a5a),
            "a refused snapshot still wrote into the kernel buffer"
        );
        Ok(())
    }

    /// Every reported field is live and self-consistent, the kernel task's row
    /// matches its scheduler state, and empty slots are all-zero.
    pub fn snapshot_fields_sane() -> Result<(), String> {
        fresh();
        let words = snapshot()?;

        check!(
            words[sysinfo::H_TICKS] == task::ticks(),
            "tick word {} is not the live clock {}",
            words[sysinfo::H_TICKS],
            task::ticks()
        );
        let total = words[sysinfo::H_FRAMES_TOTAL];
        let live = words[sysinfo::H_FRAMES_LIVE];
        let free = words[sysinfo::H_FRAMES_FREE];
        check!(total > 0, "the allocator reports zero frames");
        check!(
            free + live == total,
            "frame accounting is inconsistent: free {free} + live {live} != total {total}"
        );
        check!(
            words[sysinfo::H_FRAMES_ALLOCATED] >= live
                && words[sysinfo::H_FRAMES_FREED] <= words[sysinfo::H_FRAMES_ALLOCATED],
            "frame counters are inconsistent: live {live} allocated {} freed {}",
            words[sysinfo::H_FRAMES_ALLOCATED],
            words[sysinfo::H_FRAMES_FREED]
        );
        check!(
            words[sysinfo::H_SLAB_PEAK] >= words[sysinfo::H_SLAB_LIVE],
            "slab peak {} is below live {}",
            words[sysinfo::H_SLAB_PEAK],
            words[sysinfo::H_SLAB_LIVE]
        );
        check!(
            words[sysinfo::H_HEAP_USED] + words[sysinfo::H_HEAP_FREE]
                == words[sysinfo::H_HEAP_TOTAL]
                && words[sysinfo::H_HEAP_TOTAL] > 15 * 1024 * 1024
                && words[sysinfo::H_HEAP_TOTAL] <= mem::HEAP_SIZE,
            "heap words are inconsistent: used {} + free {} != total {}",
            words[sysinfo::H_HEAP_USED],
            words[sysinfo::H_HEAP_FREE],
            words[sysinfo::H_HEAP_TOTAL]
        );
        check!(
            words[sysinfo::H_TASKS_LIVE] == 1,
            "live task count is {}, expected the kernel task only",
            words[sysinfo::H_TASKS_LIVE]
        );

        // The kernel task's row: slot 0, pid 0, no parent, runnable, in the
        // interactive class with its default weight, named `kernel`.
        let base = sysinfo::HEADER_WORDS;
        check!(
            words[base + sysinfo::R_PRESENT] == 1
                && words[base + sysinfo::R_PID] == 0
                && words[base + sysinfo::R_PPID] == 0,
            "the kernel row presence/pid/ppid words are wrong"
        );
        check!(
            words[base + sysinfo::R_STATE] == sysinfo::state::RUNNABLE,
            "the kernel row state is {}, expected runnable",
            words[base + sysinfo::R_STATE]
        );
        check!(
            words[base + sysinfo::R_CLASS] == sysinfo::class::INTERACTIVE
                && words[base + sysinfo::R_WEIGHT] == 4,
            "the kernel row class/weight words are {} and {}",
            words[base + sysinfo::R_CLASS],
            words[base + sysinfo::R_WEIGHT]
        );
        check!(
            words[base + sysinfo::R_NAME8] == u64::from_le_bytes(*b"kernel\0\0"),
            "the kernel row short name is {:#x}",
            words[base + sysinfo::R_NAME8]
        );
        check!(
            words[base + sysinfo::R_NAME_HASH] == sysinfo::fnv1a64(b"kernel"),
            "the kernel row name hash is {:#x}",
            words[base + sysinfo::R_NAME_HASH]
        );

        // An empty slot's whole row stays zero, so a reader can rely on
        // presence alone.
        let empty = sysinfo::HEADER_WORDS + sysinfo::TASK_ROW_WORDS;
        check!(
            words[empty..empty + sysinfo::TASK_ROW_WORDS]
                .iter()
                .all(|word| *word == 0),
            "an empty task slot has non-zero row words"
        );
        Ok(())
    }

    /// A forked task shows up with its pid, ppid, name and CPU ticks, and the
    /// live count follows the table.
    pub fn snapshot_reflects_spawned_task() -> Result<(), String> {
        fresh();
        let slot = task::spawn_fork().map_err(to_string)?;
        check!(
            task::process::ppid_of(slot) == task::KERNEL_TASK,
            "the fork's ppid is {}",
            task::process::ppid_of(slot)
        );
        // Charge the child a CPU tick exactly as a timer tick would.
        task::harness::switch_current(slot);
        task::harness::simulate_tick();
        task::harness::switch_current(task::KERNEL_TASK);

        let words = snapshot()?;
        check!(
            words[sysinfo::H_TASKS_LIVE] == 2,
            "live task count is {}, expected 2",
            words[sysinfo::H_TASKS_LIVE]
        );
        let base = sysinfo::HEADER_WORDS + slot * sysinfo::TASK_ROW_WORDS;
        check!(
            words[base + sysinfo::R_PRESENT] == 1
                && words[base + sysinfo::R_PID] == slot as u64
                && words[base + sysinfo::R_PPID] == task::KERNEL_TASK as u64,
            "the spawned row presence/pid/ppid words are wrong"
        );
        check!(
            words[base + sysinfo::R_STATE] == sysinfo::state::RUNNABLE,
            "the spawned row state is {}",
            words[base + sysinfo::R_STATE]
        );
        check!(
            words[base + sysinfo::R_CPU_TICKS] >= 1,
            "the spawned row CPU ticks are {}",
            words[base + sysinfo::R_CPU_TICKS]
        );
        check!(
            words[base + sysinfo::R_NAME8] == u64::from_le_bytes(*b"fork\0\0\0\0")
                && words[base + sysinfo::R_NAME_HASH] == sysinfo::fnv1a64(b"fork"),
            "the spawned row name words are wrong"
        );
        task::harness::reset();
        Ok(())
    }

    /// Many snapshots under fork/exit/reclaim churn: the layout stays stable
    /// and neither the frame allocator nor the slab allocator grows.
    pub fn soak_snapshot_task_churn() -> Result<(), String> {
        fresh();
        const ROUNDS: usize = 96;

        // Warm up the address-space-derived tables (BUMPS, signal dispositions)
        // once, so the baseline is measured after their one-time growth.
        let warm = task::spawn_fork().map_err(to_string)?;
        task::harness::switch_current(warm);
        task::harness::simulate_tick();
        task::harness::finish(warm, 0);
        task::harness::switch_current(warm);
        task::harness::simulate_tick();
        task::reclaim_pending();
        fresh();

        let baseline = snapshot()?;
        let frames_before = mem::frame_stats().live();
        let slab_before = mem::slab::stats().live_bytes;
        for round in 0..ROUNDS {
            let slot =
                task::spawn_fork().map_err(|error| format!("round {round}: fork: {error}"))?;
            task::harness::switch_current(slot);
            task::harness::simulate_tick();
            task::harness::finish(slot, round as u64);
            task::harness::switch_current(slot);
            task::harness::simulate_tick();
            task::reclaim_pending();

            let words = snapshot()?;
            check!(
                words[sysinfo::H_VERSION] == sysinfo::SYSTEM_STATS_VERSION
                    && words[sysinfo::H_WORDS] == sysinfo::WORDS as u64
                    && words[sysinfo::H_TASK_ROW_WORDS] == sysinfo::TASK_ROW_WORDS as u64,
                "round {round}: snapshot layout changed"
            );
            check!(
                words[sysinfo::H_TASKS_LIVE] == 1,
                "round {round}: {} live tasks after reclaim",
                words[sysinfo::H_TASKS_LIVE]
            );
        }

        // Every churned address space must be gone: no frame or slab growth
        // across 96 fork/exit/reclaim generations.
        let frames_after = mem::frame_stats().live();
        check!(
            frames_after == frames_before,
            "frame leak across {ROUNDS} generations: {frames_before} -> {frames_after}"
        );
        let slab_after = mem::slab::stats().live_bytes;
        check!(
            slab_after == slab_before,
            "slab leak across {ROUNDS} generations: {slab_before} -> {slab_after}"
        );
        check!(
            baseline[sysinfo::H_TASKS_LIVE] == 1,
            "the kernel task vanished before the soak"
        );
        fresh();
        Ok(())
    }
}
