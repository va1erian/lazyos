//! Frame allocator, address-space construction and copy-on-write fork.
//!
//! Covers the frame allocator's public API, page-table/address-space
//! construction, copy-on-write fork (including a soak loop), VMA
//! split/merge/protect, and demand-zero mappings. Allocator-internals
//! tests (e.g. the counters reworked by #54) belong here so they can be
//! added without touching the harness.

use super::*;

mod fault_storm;
mod frames_and_cow;
mod layout;
mod vma;

pub(super) use fault_storm::*;
pub(super) use frames_and_cow::*;
pub(super) use layout::*;
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
];
