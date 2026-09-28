//! Regression tests for the findings of the review of the pull
//! requests that merged without a CodeRabbit pass. Each test is
//! written against the syscall or subsystem surface the bug was
//! reachable through, so it fails on the code as it was merged.
//! Covers user pointers, credential inheritance, task teardown,
//! display grant privilege, epoll nesting, ext2 short writes, and the
//! VFS search bit.

use super::*;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{Filesystem, FsError, Id, Vfs};
use crate::ipc::credentials::{self, Cred};
use crate::ipc::{channels, handles, shared};
use crate::process::cred_op;
use crate::quota::{self, Resource};
use alloc::string::ToString;
use alloc::sync::Arc;
use libmessenger::{flags, Header, Parcel, VERSION};

/// Scratch user space for the tests that need real user mappings.
const SPACE: u64 = 0x0040_0000;

const SPACE_PAGES: u64 = 8;

const EPERM: i64 = 1;

const EFAULT: i64 = 14;

/// Two's-complement `-errno`, the syscall error encoding.
fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// An unprivileged session user: no capabilities.
fn alice() -> Cred {
    Cred::new(1000, 1000, 0, 0, 7)
}

/// Turns pointer validation on for the guard's lifetime. The suite's other
/// tests pass kernel buffers as "user" pointers, so validation is off by
/// default under `lazyos_tests`.
struct Strict(bool);

impl Strict {
    fn on() -> Strict {
        Strict(crate::user_ptr::set_trust_kernel_pointers(false))
    }
}

impl Drop for Strict {
    fn drop(&mut self) {
        crate::user_ptr::set_trust_kernel_pointers(self.0);
    }
}

/// Bring-up state: kernel task current, root credentials everywhere, no
/// handles, channels, buffers, quotas or display grant.
fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    channels::reset();
    shared::reset();
    quota::reset();
    for slot in 0..task::MAX_TASKS {
        handles::reset_for_task(slot);
        credentials::reset_for_task(slot);
    }
    Ok(())
}

/// Run `f` with [`SPACE`] mapped into a fresh address space installed as
/// CR3, exactly as a syscall from a user task would find it.
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

/// A kernel-heap buffer of `len` bytes filled with a canary.
fn canary(len: usize) -> Vec<u8> {
    vec![0xA5u8; len]
}

fn untouched(buffer: &[u8], what: &str) -> Result<(), String> {
    check!(
        buffer.iter().all(|byte| *byte == 0xA5),
        "{what} wrote through a kernel pointer"
    );
    Ok(())
}

mod epoll_and_fs;
mod pointer_validation;
mod teardown_and_credentials;

pub(super) use epoll_and_fs::*;
pub(super) use pointer_validation::*;
pub(super) use teardown_and_credentials::*;

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "hardening_native_syscalls_reject_kernel_pointers",
        native_syscalls_reject_kernel_pointers,
    ),
    (
        "hardening_valid_user_buffers_still_work",
        valid_user_buffers_still_work,
    ),
    (
        "hardening_linux_abi_kernel_pointers_are_refused",
        linux_abi_kernel_pointers_are_refused,
    ),
    ("hardening_user_ptr_edge_cases", user_ptr_edge_cases),
    (
        "hardening_soak_user_ptr_validation",
        soak_user_ptr_validation,
    ),
    (
        "hardening_display_bad_pointers_leave_no_state",
        display_bad_pointers_leave_no_state,
    ),
    (
        "hardening_display_bind_requires_capability",
        display_bind_requires_capability,
    ),
    (
        "hardening_children_inherit_credentials",
        children_inherit_credentials,
    ),
    (
        "hardening_teardown_releases_fabric_state",
        teardown_releases_fabric_state,
    ),
    (
        "hardening_soak_teardown_generations",
        soak_teardown_generations,
    ),
    (
        "hardening_user_memory_quota_released_on_exit",
        user_memory_quota_released_on_exit,
    ),
    (
        "hardening_concurrent_clients_are_not_a_deadlock",
        concurrent_clients_are_not_a_deadlock,
    ),
    (
        "hardening_intern_service_names_are_bounded",
        intern_service_names_are_bounded,
    ),
    (
        "hardening_vfs_parent_directory_needs_search_bit",
        vfs_parent_directory_needs_search_bit,
    ),
    (
        "hardening_ext2_short_write_persists_and_owner_is_checked",
        ext2_short_write_persists_and_owner_is_checked,
    ),
    (
        "hardening_ext2_failed_write_does_not_expose_a_stale_block",
        ext2_failed_write_does_not_expose_a_stale_block,
    ),
    (
        "hardening_soak_ext2_short_writes_do_not_leak",
        soak_ext2_short_writes_do_not_leak,
    ),
    // Last: on the code as merged this one recurses until the kernel stack
    // overflows, which would take the rest of the suite with it.
    (
        "hardening_epoll_rejects_self_and_cyclic_registration",
        epoll_rejects_self_and_cyclic_registration,
    ),
    // Also crashes the kernel before this PR's `depth_above` fix: keep it
    // last too, right after the sibling test above.
    (
        "hardening_epoll_rejects_bottom_up_growth",
        epoll_rejects_bottom_up_growth,
    ),
];
