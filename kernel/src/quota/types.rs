//! Quota vocabulary: resources, limit tables, errors and the per-uid ledger.

use alloc::string::String;
use core::fmt;

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
    /// Device claims held by the uid (issue #240).
    DeviceClaims,
    /// Bytes of contiguous DMA pool memory held by the uid (issue #241).
    DmaMemory,
}

impl Resource {
    /// Number of resources; sizes every per-resource array and the ABI block.
    pub const COUNT: usize = 9;
    /// Every resource, in discriminant order (the ABI order).
    pub const ALL: [Resource; Resource::COUNT] = [
        Resource::KernelMemory,
        Resource::UserMemory,
        Resource::Handles,
        Resource::Fds,
        Resource::QueueBytes,
        Resource::QueueDepth,
        Resource::CpuTicks,
        Resource::DeviceClaims,
        Resource::DmaMemory,
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
            Resource::DeviceClaims => "device claims",
            Resource::DmaMemory => "DMA memory",
        }
    }

    /// Unit the usage/limit numbers are counted in.
    pub const fn unit(self) -> &'static str {
        match self {
            Resource::KernelMemory
            | Resource::UserMemory
            | Resource::QueueBytes
            | Resource::DmaMemory => "bytes",
            Resource::Handles => "handles",
            Resource::Fds => "fds",
            Resource::QueueDepth => "messages",
            Resource::CpuTicks => "ticks",
            Resource::DeviceClaims => "claims",
        }
    }
}

/// The default limit table for a regular uid. The memory limits come from
/// [`crate::limits`] (derived from RAM and the screen, overridable in
/// `lazyos.cfg`); the rest are fixed policy:
///
/// * kernel memory: `limit.quota_kernel_memory` (32 MiB on a 256 MiB guest);
/// * user memory: `limit.quota_user_memory` (256 MiB on a 256 MiB guest);
/// * 1024 handles;
/// * fds: four full descriptor tables (`limit.fd_max` each);
/// * 4 MiB / 1024 messages of Messenger queueing;
/// * 2^32 CPU ticks (about 497 days at 100 Hz, i.e. effectively "metered, not
///   capped" until CPU shares get a real policy);
/// * 8 device claims;
/// * DMA memory: half the DMA pool, at least 8 MiB.
pub fn default_limits_regular() -> [u64; Resource::COUNT] {
    use crate::limits::{self, Id};
    let pool = limits::dma_pool_bytes(limits::ram_bytes());
    [
        limits::get(Id::QuotaKernelMemory),
        limits::get(Id::QuotaUserMemory),
        1024,
        limits::get(Id::FdMax).saturating_mul(4),
        4 << 20,
        1024,
        1 << 32,
        8,
        (pool / 2).max(8 << 20),
    ]
}

/// The limit table for uid 0. The kernel task and bring-up children run as root
/// before any login, so root is metered but not capped; a real policy source
/// replaces this with [`set_limit`] once profiles are compiled.
pub const ROOT_LIMITS: [u64; Resource::COUNT] = [u64::MAX; Resource::COUNT];

/// The default limits for `uid`: root is uncapped, everyone else gets
/// [`default_limits_regular`].
pub(super) fn default_limits(uid: u32) -> [u64; Resource::COUNT] {
    if uid == 0 {
        ROOT_LIMITS
    } else {
        default_limits_regular()
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
pub(super) struct Entry {
    pub(super) uid: u32,
    pub(super) limits: [u64; Resource::COUNT],
    pub(super) usage: [u64; Resource::COUNT],
    pub(super) peak: [u64; Resource::COUNT],
    pub(super) charges: u64,
    pub(super) releases: u64,
    pub(super) denials: u64,
    pub(super) over_releases: u64,
}

impl Entry {
    pub(super) fn new(uid: u32) -> Self {
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
