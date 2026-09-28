//! Frame allocator, address-space construction and copy-on-write fork.
//!
//! Covers the frame allocator's public API, page-table/address-space
//! construction, copy-on-write fork (including a soak loop), VMA
//! split/merge/protect, and demand-zero mappings. Allocator-internals
//! tests (e.g. the counters reworked by #54) belong here so they can be
//! added without touching the harness.

use super::*;

mod frames_and_cow;
mod vma;

pub(super) use frames_and_cow::*;
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
    ("mem_vma_split_merge_protect", vma_split_merge_protect),
    ("mem_demand_zero_and_munmap", demand_zero_and_munmap),
    ("mem_vma_cow_mprotect", vma_cow_mprotect),
];
