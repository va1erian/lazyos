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

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use spin::Mutex;

/// Kinds of metered resource. The discriminant order is the wire order of the
/// syscall-11 stats block, so append new resources at the end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resource {
    /// Kernel frames/objects held for the uid.
    KernelMemory,
    /// User address-space bytes (VMA growth).
    UserMemory,
    /// Messenger handles held across the uid's tasks.
    Handles,
    /// File descriptors (API only until the fd table is charged; see module docs).
    Fds,
    /// Parcel bytes queued in Messenger inboxes.
    QueueBytes,
    /// Messages queued in Messenger inboxes.
    QueueDepth,
    /// CPU ticks.
    CpuTicks,
}

impl Resource {
    /// Number of resources; sizes every per-resource array and the ABI block.
    pub const COUNT: usize = 7;
    /// Every resource, in discriminant order (the ABI order).
    pub const ALL: [Resource; Resource::COUNT] = [
        Resource::KernelMemory,
        Resource::UserMemory,
        Resource::Handles,
        Resource::Fds,
        Resource::QueueBytes,
        Resource::QueueDepth,
        Resource::CpuTicks,
    ];

    /// Index into the per-resource arrays.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Resource name for friendly errors.
    pub const fn name(self) -> &'static str {
        match self {
            Resource::KernelMemory => "kernel memory",
            Resource::UserMemory => "user memory",
            Resource::Handles => "Messenger handles",
            Resource::Fds => "file descriptors",
            Resource::QueueBytes => "queued Messenger bytes",
            Resource::QueueDepth => "queued Messenger messages",
            Resource::CpuTicks => "CPU",
        }
    }

    /// Unit the usage/limit numbers are counted in.
    pub const fn unit(self) -> &'static str {
        match self {
            Resource::KernelMemory | Resource::UserMemory | Resource::QueueBytes => "bytes",
            Resource::Handles => "handles",
            Resource::Fds => "fds",
            Resource::QueueDepth => "messages",
            Resource::CpuTicks => "ticks",
        }
    }
}

/// The default limit table for a regular uid, documented per resource:
/// 32 MiB of kernel memory, 256 MiB of user memory, 1024 handles, 256 fds,
/// 4 MiB / 1024 messages of Messenger queueing, and 2^32 CPU ticks (about
/// 497 days at 100 Hz, i.e. effectively "metered, not capped" until CPU shares
/// get a real policy).
pub const DEFAULT_LIMITS: [u64; Resource::COUNT] = [
    32 << 20,  // kernel memory
    256 << 20, // user memory
    1024,      // handles
    256,       // fds
    4 << 20,   // queued bytes
    1024,      // queued messages
    1 << 32,   // CPU ticks
];

/// The limit table for uid 0. The kernel task and bring-up children run as root
/// before any login, so root is metered but not capped; a real policy source
/// replaces this with [`set_limit`] once profiles are compiled.
pub const ROOT_LIMITS: [u64; Resource::COUNT] = [u64::MAX; Resource::COUNT];

/// The default limits for `uid`: root is uncapped, everyone else gets
/// [`DEFAULT_LIMITS`].
const fn default_limits(uid: u32) -> [u64; Resource::COUNT] {
    if uid == 0 {
        ROOT_LIMITS
    } else {
        DEFAULT_LIMITS
    }
}

/// A refused charge: which resource, whose, and how full it was. The friendly
/// [`QuotaError::message`] carries the same numbers for userspace.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct QuotaError {
    /// User the charge was for.
    pub uid: u32,
    /// Resource that ran out.
    pub resource: Resource,
    /// Live usage before the refused charge.
    pub usage: u64,
    /// Configured limit.
    pub limit: u64,
}

impl QuotaError {
    /// The friendly, user-facing explanation: resource name plus current usage
    /// and limit, matching the convention in `docs/security-model.md` (never a
    /// bare `EPERM`/`ENOSPC`).
    pub fn message(&self) -> String {
        alloc::format!(
            "uid {} is over its {} quota: {} of {} {} in use",
            self.uid,
            self.resource.name(),
            self.usage,
            self.limit,
            self.resource.unit()
        )
    }
}

impl fmt::Debug for QuotaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "QuotaError({}, {:?}, {}/{})",
            self.uid, self.resource, self.usage, self.limit
        )
    }
}

impl fmt::Display for QuotaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// A full snapshot of one uid's ledger, returned by [`stats`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stats {
    /// User this record is for.
    pub uid: u32,
    /// Live usage per resource, indexed by [`Resource::index`].
    pub usage: [u64; Resource::COUNT],
    /// Limits per resource. A uid with no charges yet reads its defaults.
    pub limits: [u64; Resource::COUNT],
    /// High-water mark of each usage counter.
    pub peak: [u64; Resource::COUNT],
    /// Successful charges.
    pub charges: u64,
    /// Release calls (a release is always counted, even when it saturates).
    pub releases: u64,
    /// Refused charges.
    pub denials: u64,
    /// Releases that exceeded live usage and saturated at zero (should not
    /// happen in balanced code; a nonzero value points at an accounting bug).
    pub over_releases: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            uid: 0,
            usage: [0; Resource::COUNT],
            limits: [0; Resource::COUNT],
            peak: [0; Resource::COUNT],
            charges: 0,
            releases: 0,
            denials: 0,
            over_releases: 0,
        }
    }
}

/// One uid's ledger.
struct Entry {
    uid: u32,
    limits: [u64; Resource::COUNT],
    usage: [u64; Resource::COUNT],
    peak: [u64; Resource::COUNT],
    charges: u64,
    releases: u64,
    denials: u64,
    over_releases: u64,
}

impl Entry {
    fn new(uid: u32) -> Self {
        Entry {
            uid,
            limits: default_limits(uid),
            usage: [0; Resource::COUNT],
            peak: [0; Resource::COUNT],
            charges: 0,
            releases: 0,
            denials: 0,
            over_releases: 0,
        }
    }
}

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
    SPACES.lock().clear();
}

/// User-memory bytes each address space currently holds a charge for, keyed by
/// the space's PML4 and the uid that was charged.
///
/// The charge is per uid, but the *lifetime* is per address space: the bytes a
/// task `mmap`ed or `brk`ed are still charged when it exits without unmapping
/// them, so teardown must give them back ([`forget_address_space`]), and to
/// the uid that was actually charged even if the task's identity changed since.
/// `SPACES` is a leaf lock taken after `QUOTAS` is released, never nested in it.
struct SpaceCharge {
    table: u64,
    uid: u32,
    bytes: u64,
}

static SPACES: Mutex<Vec<SpaceCharge>> = Mutex::new(Vec::new());

/// The address space charges against `slot` are recorded under: `slot`'s own
/// PML4, not whatever table happens to be active on the CPU right now.
///
/// A syscall handler runs on its caller's table, so for the common case
/// (`slot == task::current()`) the two agree; but `charge_for_slot`/
/// `release_for_slot` take an explicit slot precisely so a caller (or a test)
/// can act on a *different* task, and the active CR3 would then key the charge
/// under the wrong address space -- `forget_address_space` could never release
/// it when that space is freed, and a release could wrongly drain whatever
/// space happened to be active. A slot with no task yet (a narrow window
/// during spawn) falls back to the active table, matching the old behaviour.
fn space_of(slot: usize) -> u64 {
    crate::task::pml4_of(slot).unwrap_or_else(|| crate::mem::kernel_table().as_u64())
}

/// Charge `delta` bytes of user memory to `slot`'s uid and record it against
/// `slot`'s own address space.
fn charge_user_memory(slot: usize, delta: u64) -> Result<(), QuotaError> {
    let uid = crate::ipc::credentials::of(slot).uid;
    charge(uid, Resource::UserMemory, delta)?;
    let table = space_of(slot);
    let mut spaces = SPACES.lock();
    match spaces
        .iter_mut()
        .find(|space| space.table == table && space.uid == uid)
    {
        Some(space) => space.bytes = space.bytes.saturating_add(delta),
        None => spaces.push(SpaceCharge {
            table,
            uid,
            bytes: delta,
        }),
    }
    Ok(())
}

/// Release up to `delta` bytes of `slot`'s address space's user-memory
/// charge, preferring the caller's current uid. Bytes the space never charged
/// (ELF segments and stacks are mapped before a uid exists) release nothing:
/// they must not eat into another task's live usage.
fn release_user_memory(slot: usize, delta: u64) {
    let current = crate::ipc::credentials::of(slot).uid;
    let table = space_of(slot);
    let mut remaining = delta;
    let mut releases: Vec<(u32, u64)> = Vec::new();
    {
        let mut spaces = SPACES.lock();
        for prefer_current in [true, false] {
            for space in spaces
                .iter_mut()
                .filter(|space| space.table == table && (space.uid == current) == prefer_current)
            {
                let take = space.bytes.min(remaining);
                if take > 0 {
                    space.bytes -= take;
                    remaining -= take;
                    releases.push((space.uid, take));
                }
            }
        }
        spaces.retain(|space| space.bytes > 0);
    }
    for (uid, bytes) in releases {
        release(uid, Resource::UserMemory, bytes);
    }
}

/// Give back every user-memory byte the address space `table` still holds a
/// charge for. Called when the space is freed (last user reaped), so a task
/// that exits without unmapping cannot strand its uid's quota.
pub fn forget_address_space(table: u64) {
    let mut freed: Vec<(u32, u64)> = Vec::new();
    {
        let mut spaces = SPACES.lock();
        spaces.retain(|space| {
            if space.table == table {
                freed.push((space.uid, space.bytes));
                false
            } else {
                true
            }
        });
    }
    for (uid, bytes) in freed {
        release(uid, Resource::UserMemory, bytes);
    }
}

/// Charge `resource` to the uid stamped on task `slot`.
///
/// The uid is read from [`crate::ipc::credentials`] under `CREDS`, then the
/// quota lock is taken (`CREDS` -> `QUOTAS`, the documented order).
/// [`Resource::UserMemory`] is additionally recorded per address space (see
/// [`SpaceCharge`]).
pub fn charge_for_slot(slot: usize, resource: Resource, delta: u64) -> Result<(), QuotaError> {
    if resource == Resource::UserMemory {
        return charge_user_memory(slot, delta);
    }
    let uid = crate::ipc::credentials::of(slot).uid;
    charge(uid, resource, delta)
}

/// Release `resource` from the uid stamped on task `slot`; see [`release`].
pub fn release_for_slot(slot: usize, resource: Resource, delta: u64) {
    if resource == Resource::UserMemory {
        return release_user_memory(slot, delta);
    }
    let uid = crate::ipc::credentials::of(slot).uid;
    release(uid, resource, delta);
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
