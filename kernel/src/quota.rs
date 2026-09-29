//! Per-uid resource quotas (issue #103).
//!
//! `docs/security-model.md` section 5, item 5 promises that memory, fds,
//! handles, IPC queue depth and CPU ticks are metered per user and that
//! exceeding a limit is a friendly `ERR_QUOTA` carrying the current usage and
//! the limit. This module is the kernel side of that promise: one small table
//! keyed by uid, with a *limit* and a *live usage* counter per resource, plus a
//! `check`/`charge`/`release` API.
//!
//! # Resources
//!
//! * [`Resource::KernelMemory`] -- kernel frames/objects held for the uid
//!   (shared-buffer frames today; slab objects as their call sites land).
//! * [`Resource::UserMemory`] -- user address-space bytes (VMA growth through
//!   `mmap`/`brk`; segments and stacks are mapped before a uid exists, so the
//!   accounting is deliberately coarse).
//! * [`Resource::Handles`] -- Messenger handles held across every task of the
//!   uid. The per-process [`crate::ipc::handles::MAX_HANDLES`] check stays the
//!   first line; this is the aggregate.
//! * [`Resource::Fds`] -- file descriptors. The resource and the API exist now,
//!   but the fd table lives in `task/**` and its open/close/dup/fork paths
//!   cannot be charged without editing that module, so enforcement lands with
//!   the fd-table refactor (documented follow-up).
//! * [`Resource::QueueBytes`] / [`Resource::QueueDepth`] -- parcels parked in
//!   Messenger inboxes, charged to the *sender's* uid when queued and released
//!   when delivered or dropped.
//! * [`Resource::CpuTicks`] -- CPU ticks. The API exists and the suite exercises
//!   it; wiring it to the scheduler's accounting is the same `task/**` follow-up
//!   (the scheduler currently charges ticks to slots, not uids).
//!
//! # Limits
//!
//! A fresh uid gets [`DEFAULT_LIMITS`]; uid 0 (the system/root user) gets
//! [`ROOT_LIMITS`] because bring-up and the kernel task predate login. Limits
//! are kernel policy: [`set_limit`] is the API a future profile loader calls,
//! setting limits from userspace is out of scope. A per-uid entry is created on
//! first charge and remembers its limits and peak usage.
//!
//! # Introspection
//!
//! [`stats`] snapshots one uid, [`stats_words`] encodes the same snapshot in
//! the native ABI block read by syscall 11 (`process::sys_quota`), and the
//! kernel suite calls both through [`crate::process`]'s test dispatch hook.
//!
//! # Locking
//!
//! `QUOTAS` is a leaf lock: no path takes it and then another kernel lock.
//! [`charge_for_slot`]/[`release_for_slot`] read the slot's stamped uid first
//! (`CREDS` -> `QUOTAS`), so the documented order is credentials, then quotas.
//! Callers that already hold a registry lock (channels, shared buffers) take
//! `QUOTAS` last, which is consistent with its leaf position.

#![allow(dead_code)] // Enforcement call sites land incrementally; the suite exercises the full API today.

use alloc::vec::Vec;
use spin::Mutex;

mod space;
mod types;

pub use space::{charge_for_slot, forget_address_space, release_for_slot};
use types::{default_limits, Entry};
#[allow(unused_imports)] // the full limit tables stay part of the module API
pub use types::{QuotaError, Resource, Stats, DEFAULT_LIMITS, ROOT_LIMITS};

/// The quota table. A linear scan is right at this scale (a handful of logins).
static QUOTAS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Borrow `uid`'s ledger, creating it with the default limits on first use.
fn entry_for(quotas: &mut Vec<Entry>, uid: u32) -> &mut Entry {
    if let Some(index) = quotas.iter().position(|entry| entry.uid == uid) {
        return &mut quotas[index];
    }
    quotas.push(Entry::new(uid));
    let last = quotas.len() - 1;
    &mut quotas[last]
}

/// Whether `usage + delta` fits under `limit`, saturating so a request near
/// `u64::MAX` cannot wrap past the check.
fn fits(usage: u64, limit: u64, delta: u64) -> bool {
    usage.saturating_add(delta) <= limit
}

/// The configured limit for `(uid, resource)`; a uid with no ledger yet reads
/// its default.
pub fn limit(uid: u32, resource: Resource) -> u64 {
    let quotas = QUOTAS.lock();
    quotas
        .iter()
        .find(|entry| entry.uid == uid)
        .map(|entry| entry.limits[resource.index()])
        .unwrap_or(default_limits(uid)[resource.index()])
}

/// Live usage for `(uid, resource)`; an unknown uid reads zero.
pub fn usage(uid: u32, resource: Resource) -> u64 {
    let quotas = QUOTAS.lock();
    quotas
        .iter()
        .find(|entry| entry.uid == uid)
        .map(|entry| entry.usage[resource.index()])
        .unwrap_or(0)
}

/// Whether `delta` more of `resource` fits in `uid`'s quota, without charging.
///
/// This is the cheap pre-flight check; choke points that must not race their
/// own charge use [`charge`] (check and apply under one lock).
pub fn check(uid: u32, resource: Resource, delta: u64) -> Result<(), QuotaError> {
    let quotas = QUOTAS.lock();
    let (usage, limit) = match quotas.iter().find(|entry| entry.uid == uid) {
        Some(entry) => (
            entry.usage[resource.index()],
            entry.limits[resource.index()],
        ),
        None => (0, default_limits(uid)[resource.index()]),
    };
    if fits(usage, limit, delta) {
        Ok(())
    } else {
        Err(QuotaError {
            uid,
            resource,
            usage,
            limit,
        })
    }
}

/// Atomically check and apply a charge of `delta` of `resource` to `uid`.
///
/// Returns [`QuotaError`] (with the numbers for the friendly message) when the
/// charge would exceed the limit, leaving usage unchanged.
pub fn charge(uid: u32, resource: Resource, delta: u64) -> Result<(), QuotaError> {
    charge_many(uid, &[(resource, delta)])
}

/// Atomically charge several resources of one uid against one locked ledger.
///
/// Either every charge fits and is applied, or the first that does not is
/// returned and nothing changes. Choke points that meter more than one resource
/// for the same event (e.g. queue bytes *and* queue depth) use this so a
/// partial charge can never leak.
pub fn charge_many(uid: u32, charges: &[(Resource, u64)]) -> Result<(), QuotaError> {
    let mut quotas = QUOTAS.lock();
    let entry = entry_for(&mut quotas, uid);
    for &(resource, delta) in charges {
        let index = resource.index();
        if !fits(entry.usage[index], entry.limits[index], delta) {
            entry.denials += 1;
            return Err(QuotaError {
                uid,
                resource,
                usage: entry.usage[index],
                limit: entry.limits[index],
            });
        }
    }
    for &(resource, delta) in charges {
        let index = resource.index();
        entry.usage[index] = entry.usage[index].saturating_add(delta);
        entry.peak[index] = entry.peak[index].max(entry.usage[index]);
        entry.charges += 1;
    }
    Ok(())
}

/// Release `delta` of `resource` from `uid`'s ledger, saturating at zero.
///
/// A release for an unknown uid is a no-op; a release larger than the live
/// usage saturates and bumps [`Stats::over_releases`] instead of wrapping.
pub fn release(uid: u32, resource: Resource, delta: u64) {
    release_many(uid, &[(resource, delta)]);
}

/// Release several resources of one uid against one locked ledger; see
/// [`release`].
pub fn release_many(uid: u32, releases: &[(Resource, u64)]) {
    let mut quotas = QUOTAS.lock();
    let Some(entry) = quotas.iter_mut().find(|entry| entry.uid == uid) else {
        return;
    };
    for &(resource, delta) in releases {
        let index = resource.index();
        if delta > entry.usage[index] {
            entry.over_releases += 1;
            entry.usage[index] = 0;
        } else {
            entry.usage[index] -= delta;
        }
        entry.releases += 1;
    }
}

/// Set `uid`'s limit for `resource`, creating the ledger if needed.
///
/// Kernel-only policy API: profiles/init call it; no syscall reaches it (the
/// userspace-facing call, syscall 11, is read-only).
pub fn set_limit(uid: u32, resource: Resource, limit: u64) {
    let mut quotas = QUOTAS.lock();
    entry_for(&mut quotas, uid).limits[resource.index()] = limit;
}

/// Snapshot `uid`'s ledger. A uid with no ledger reads its defaults with zero
/// usage.
pub fn stats(uid: u32) -> Stats {
    let quotas = QUOTAS.lock();
    match quotas.iter().find(|entry| entry.uid == uid) {
        Some(entry) => Stats {
            uid,
            usage: entry.usage,
            limits: entry.limits,
            peak: entry.peak,
            charges: entry.charges,
            releases: entry.releases,
            denials: entry.denials,
            over_releases: entry.over_releases,
        },
        None => Stats {
            uid,
            limits: default_limits(uid),
            ..Stats::default()
        },
    }
}

/// Snapshot every uid that has a ledger, for tools/tests.
pub fn all_stats() -> Vec<Stats> {
    let quotas = QUOTAS.lock();
    quotas
        .iter()
        .map(|entry| Stats {
            uid: entry.uid,
            usage: entry.usage,
            limits: entry.limits,
            peak: entry.peak,
            charges: entry.charges,
            releases: entry.releases,
            denials: entry.denials,
            over_releases: entry.over_releases,
        })
        .collect()
}

/// Drop every ledger, restoring the default tables (test isolation and a
/// future logout hook).
pub fn reset() {
    QUOTAS.lock().clear();
    space::reset();
}

/// `u64` words in the syscall-11 stats block: a `(usage, limit)` pair per
/// resource, in [`Resource::ALL`] order.
pub const STATS_WORDS: usize = Resource::COUNT * 2;

/// Encode `uid`'s usage/limits in the native ABI block read by syscall 11.
///
/// Word `2*i` is the usage and word `2*i + 1` the limit of `Resource::ALL[i]`.
pub fn stats_words(uid: u32) -> [u64; STATS_WORDS] {
    let snapshot = stats(uid);
    let mut words = [0u64; STATS_WORDS];
    for resource in Resource::ALL {
        let index = resource.index();
        words[index * 2] = snapshot.usage[index];
        words[index * 2 + 1] = snapshot.limits[index];
    }
    words
}
