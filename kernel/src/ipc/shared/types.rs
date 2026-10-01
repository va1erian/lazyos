//! Shared-buffer vocabulary: limits, flags, errors and stats snapshots.

use super::*;

/// Bytes per mapped page.
pub(super) const PAGE: u64 = 4096;
/// Largest single buffer a process may create.
pub const MAX_BUFFER_SIZE: u64 = 64 << 20;
/// Largest number of live buffers in the kernel registry.
pub const MAX_BUFFERS: usize = 256;
/// Per-process byte quota (section 9's metering, applied to buffer memory).
/// Sized for a double-buffered window (`Present`, issue #372) as large as
/// the biggest logical screen: two 1920x1080 RGBA slots are 15.8 MiB. The
/// per-uid `KernelMemory` quota still bounds what all of a user's tasks hold.
pub const MAX_BUFFER_BYTES_PER_PROCESS: u64 = 16 << 20;
/// Per-process live-buffer quota.
pub const MAX_BUFFERS_PER_PROCESS: u64 = 64;

/// Creation flags (section 10). The numeric values are kernel-internal; a
/// syscall ABI maps its own constants onto them.
pub mod flags {
    /// The holder may read through a mapping.
    pub const READ: u32 = 1 << 0;
    /// The holder may write through a mapping.
    pub const WRITE: u32 = 1 << 1;
    /// Never map the buffer into any other task: only the creator gets a
    /// mapping, so shared material (key material, DMA targets) stays out of
    /// client address spaces.
    pub const SHARE_ONLY: u32 = 1 << 2;
    /// Request executable mappings. Denied by default: the kernel has no W^X
    /// story for shared memory yet.
    pub const EXECUTABLE: u32 = 1 << 3;
    /// Pin the frames for DMA. Recorded now; used when drivers grow a DMA
    /// path that needs stable physical addresses.
    pub const PINNED: u32 = 1 << 4;
    /// Every defined flag bit; unknown bits are rejected.
    pub const ALL: u32 = READ | WRITE | SHARE_ONLY | EXECUTABLE | PINNED;
}

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
    /// A flag bit is not defined.
    BadFlags,
    /// The size is zero or over [`MAX_BUFFER_SIZE`].
    BadSize,
    /// `EXECUTABLE` was requested; executable shared memory is denied.
    ExecutableDenied,
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
    /// A `SHARE_ONLY` buffer cannot be mapped into this task.
    ShareOnly,
    /// The buffer is already gone (a stale object id).
    NotFound,
    /// A buffer descriptor's `offset`/`len` does not fit the buffer.
    BadDescriptor,
    /// A fence sequence went backwards.
    StaleSequence,
    /// The deadline passed before the fence sequence was submitted.
    TimedOut,
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
            Error::BadFlags => "unknown shared-buffer flags were requested",
            Error::BadSize => "a shared buffer must be between 1 byte and the size limit",
            Error::ExecutableDenied => "executable shared memory is denied by default",
            Error::RegistryFull => "the kernel shared-buffer registry is full",
            Error::Quota => "this process is over its shared-buffer quota",
            Error::UserQuota => "this user is over its shared-buffer memory quota",
            Error::OutOfMemory => "there is not enough free memory for this buffer",
            Error::MapFailed => "the buffer could not be mapped into this address space",
            Error::ShareOnly => "this buffer is share-only and is not mapped into this process",
            Error::NotFound => "that shared buffer no longer exists",
            Error::BadDescriptor => "the buffer descriptor's range does not fit the buffer",
            Error::StaleSequence => "the fence sequence is older than the last submitted one",
            Error::TimedOut => "the deadline passed before the fence sequence was submitted",
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
    /// Cumulative `fence_submit` calls.
    pub fences_submitted: u64,
    /// Cumulative `fence_wait` calls that had to park.
    pub fence_waits: u64,
    /// Cumulative waits that hit their deadline.
    pub fence_timeouts: u64,
    /// Fence sequences submitted but not yet observed by a waiter.
    pub outstanding_fences: u64,
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
    /// Fence waits that had to park.
    pub fence_waits: u64,
    /// Fence waits that hit their deadline.
    pub fence_timeouts: u64,
    /// That process's submissions not yet observed by a waiter.
    pub outstanding_fences: u64,
}

/// A buffer's live state, returned by [`info`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferInfo {
    pub size: u64,
    pub flags: u32,
    /// Whether the buffer is DMA-backed (its frames came from the DMA pool and
    /// its quota is `DmaMemory`; issue #241).
    pub dma: bool,
    /// Backing frames.
    pub frames: u64,
    /// Live references (handles plus in-flight messages).
    pub refs: u64,
    /// Mappings installed in task address spaces.
    pub mappings: u64,
    /// Highest fence sequence submitted.
    pub submitted: u64,
    /// Highest fence sequence observed by a waiter.
    pub waited: u64,
}
