//! Shared-buffer vocabulary: limits, errors and stats snapshots.

use super::*;

/// Bytes per mapped page.
pub(super) const PAGE: u64 = 4096;
/// Largest number of live buffers in the kernel registry.
pub const MAX_BUFFERS: usize = 256;

/// Per-process byte quota (section 9's metering, applied to buffer memory),
/// and so also the largest single buffer: `limit.shared_buffer_max`
/// (`crate::limits`). Derived from the screen so a double-buffered
/// full-screen window (`Present`, issue #372) plus the compositor's screen
/// buffer fit at any resolution, 4K included. The per-uid `KernelMemory`
/// quota still bounds what all of a user's tasks hold.
pub fn max_bytes_per_process() -> u64 {
    crate::limits::shared_buffer_max()
}
/// Per-process live-buffer quota.
pub const MAX_BUFFERS_PER_PROCESS: u64 = 64;

/// Why a shared-buffer operation failed. Messages are user-facing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The handle is unused or out of range.
    InvalidHandle,
    /// The handle exists but does not name a buffer.
    WrongKind,
    /// The handle lacks the right the operation needs.
    MissingRight,
    /// The process is holding the maximum number of handles.
    NoFreeHandle,
    /// No task exists in the current slot.
    BadTask,
    /// The size is zero or over [`max_bytes_per_process`].
    BadSize,
    /// The kernel buffer registry is full.
    RegistryFull,
    /// The process is over its buffer byte or count quota.
    Quota,
    /// The creator's *user* is over its per-uid kernel-memory quota (issue
    /// #103).
    UserQuota,
    /// No frames were available for the buffer.
    OutOfMemory,
    /// A page could not be mapped into the address space.
    MapFailed,
    /// A driver's share-only DMA buffer cannot be mapped into this task.
    ShareOnly,
    /// The buffer is already gone (a stale object id).
    NotFound,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub fn message(self) -> &'static str {
        match self {
            Error::InvalidHandle => "that Messenger handle does not exist",
            Error::WrongKind => "that handle does not name a shared buffer",
            Error::MissingRight => "this handle does not grant the right to use the buffer",
            Error::NoFreeHandle => "the process is holding too many Messenger handles",
            Error::BadTask => "no task exists in this slot",
            Error::BadSize => "a shared buffer must be between 1 byte and the size limit",
            Error::RegistryFull => "the kernel shared-buffer registry is full",
            Error::Quota => "this process is over its shared-buffer quota",
            Error::UserQuota => "this user is over its shared-buffer memory quota",
            Error::OutOfMemory => "there is not enough free memory for this buffer",
            Error::MapFailed => "the buffer could not be mapped into this address space",
            Error::ShareOnly => "this buffer is share-only and is not mapped into this process",
            Error::NotFound => "that shared buffer no longer exists",
        }
    }
}

/// Translate a handle-table error into the shared-buffer vocabulary.
pub(super) fn from_handles(error: handles::Error) -> Error {
    match error {
        handles::Error::NoFreeHandle => Error::NoFreeHandle,
        handles::Error::InvalidHandle => Error::InvalidHandle,
        handles::Error::MissingRight => Error::MissingRight,
        handles::Error::BadTask => Error::BadTask,
        // A per-uid handle-quota refusal is the same user-facing condition as
        // the per-process cap (issue #103).
        handles::Error::Quota => Error::NoFreeHandle,
    }
}

/// A snapshot of the registry's counters, for `msg_stats` and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Live buffers in the registry.
    pub buffers: u64,
    /// Bytes across live buffers.
    pub bytes: u64,
    /// Live mappings across all tasks.
    pub mappings: u64,
    /// Buffer descriptors delivered to a receiver without copying data.
    pub handoffs: u64,
}

/// One process's buffer accounting, for `msg_stats` and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessStats {
    /// Bytes charged to the process.
    pub bytes: u64,
    /// Buffers the process created.
    pub buffers: u64,
}

/// A buffer's live state, returned by [`info`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferInfo {
    pub size: u64,
    /// Whether the buffer is DMA-backed (its frames came from the DMA pool and
    /// its quota is `DmaMemory`; issue #241).
    pub dma: bool,
    /// Backing frames.
    pub frames: u64,
    /// Live references (handles plus in-flight messages).
    pub refs: u64,
    /// Mappings installed in task address spaces.
    pub mappings: u64,
}
