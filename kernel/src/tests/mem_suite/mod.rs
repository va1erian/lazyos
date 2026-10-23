//! Frame allocator, address-space construction and copy-on-write fork.
//!
//! Covers the frame allocator's public API, page-table/address-space
//! construction, copy-on-write fork (including a soak loop), VMA
//! split/merge/protect, and demand-zero mappings. Allocator-internals
//! tests (e.g. the counters reworked by #54) belong here so they can be
//! added without touching the harness.

use super::*;

mod accounting;
mod fault_storm;
mod frames_and_cow;
mod layout;
mod oom;
mod table_parents;
mod vma;

pub(super) use accounting::*;
pub(super) use fault_storm::*;
pub(super) use frames_and_cow::*;
pub(super) use layout::*;
pub(super) use oom::*;
pub(super) use table_parents::*;
pub(super) use vma::*;

pub(super) const CASES: &[(&str, Test)] = &[
    ("mem_frames_distinct_aligned", frames_distinct_aligned),
    ("mem_zeroed_frame_clear", zeroed_frame_clear),
    (
        "mem_user_table_shares_kernel_half",
        user_table_shares_kernel_half,
    ),
    ("mem_cow_clone_copies_on_write", cow_clone_copies_on_write),
    ("mem_soak_cow_fork_churn", soak_cow_fork_churn),
    ("mem_cow_sole_owner_keeps_frame", cow_sole_owner_keeps_frame),
    ("mem_vma_split_merge_protect", vma_split_merge_protect),
    ("mem_demand_zero_and_munmap", demand_zero_and_munmap),
    ("mem_vma_cow_mprotect", vma_cow_mprotect),
    (
        "mem_fault_storm_reports_once_per_storm",
        fault_storm_reports_once_per_storm,
    ),
    (
        "mem_pte_chain_walks_to_the_leaf",
        pte_chain_walks_to_the_leaf,
    ),
    (
        "mem_remapped_cow_pages_keep_writable_parents",
        remapped_cow_pages_keep_writable_parents,
    ),
    (
        "mem_mprotect_over_a_fresh_region_keeps_writable_parents",
        mprotect_over_a_fresh_region_keeps_writable_parents,
    ),
    (
        "mem_writable_leaves_repair_read_only_parents",
        writable_leaves_repair_read_only_parents,
    ),
    (
        "mem_soak_remap_after_fork_keeps_writable_parents",
        soak_remap_after_fork_keeps_writable_parents,
    ),
    ("mem_regions_merge_hostile_maps", regions_merge_hostile_maps),
    (
        "mem_regions_absurd_range_is_clamped_and_droppable",
        regions_absurd_range_is_clamped_and_droppable,
    ),
    (
        "mem_regions_keep_the_largest_when_full",
        regions_keep_the_largest_when_full,
    ),
    ("mem_regions_soak_random_maps", regions_soak_random_maps),
    (
        "mem_user_window_spans_many_entries",
        user_window_spans_many_entries,
    ),
    ("mem_high_frames_reachable", high_frames_reachable),
    ("mem_linux_stack_is_lazy", linux_stack_is_lazy),
    (
        "mem_oom_kill_is_sigkill_and_returns_frames",
        oom_kill_is_sigkill_and_returns_frames,
    ),
    ("mem_oom_refusals_stay_segv", oom_refusals_stay_segv),
    (
        "mem_oom_cow_copy_without_a_frame",
        oom_cow_copy_without_a_frame,
    ),
    (
        "mem_oom_page_table_frame_shortage",
        oom_page_table_frame_shortage,
    ),
    ("mem_soak_oom_kill_generations", soak_oom_kill_generations),
    (
        "mem_accounting_private_pages_count_in_full",
        accounting_private_pages_count_in_full,
    ),
    (
        "mem_accounting_shared_pages_are_divided",
        accounting_shared_pages_are_divided,
    ),
    (
        "mem_accounting_a_holder_is_not_a_mapper",
        accounting_a_holder_is_not_a_mapper,
    ),
    (
        "mem_accounting_classes_follow_the_vmas",
        accounting_classes_follow_the_vmas,
    ),
    (
        "mem_accounting_skips_device_leaves",
        accounting_skips_device_leaves,
    ),
    (
        "mem_accounting_window_buffers_are_divided",
        accounting_window_buffers_are_divided,
    ),
    (
        "mem_accounting_soak_forks_add_up",
        accounting_soak_forks_add_up,
    ),
];
