//! The periodic flusher: bounds how much a crash can lose.
//!
//! Cached ext2 volumes keep writes in memory until a commit
//! (`libs/ext2fs/src/commit.rs`). `sync`, `fsync`, unmount and the power path
//! commit explicitly; this flusher commits every mount at least every
//! [`INTERVAL_TICKS`] so a crash loses at most about that much work. It runs
//! from the kernel task's loop ([`service`] in `mux.rs`), never waits for a
//! busy VFS (it retries shortly instead), and leaves volumes flagged dirty:
//! only a sync marks one clean.
//!
//! It is also the memory-pressure hook: while free frames are short it asks
//! every cache to give its clean pages back after writing back.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::mem;

/// Most time between two writebacks of a mount: 5 s of 100 Hz ticks.
pub const INTERVAL_TICKS: u64 = 500;
/// Retry delay when the VFS was busy.
const RETRY_TICKS: u64 = 10;

/// The tick at which the next writeback is due.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Write back every mount when due. Cheap when it is not.
pub fn service() {
    let now = crate::task::ticks();
    if now < NEXT.load(Ordering::Relaxed) {
        return;
    }
    let pressure = under_pressure();
    let next = match super::try_with(|vfs| vfs.writeback_all(pressure)) {
        // A failure is logged by the volume and kept for the next `sync`,
        // which is where a caller learns of it.
        Some(_) => INTERVAL_TICKS,
        None => RETRY_TICKS,
    };
    NEXT.store(now + next, Ordering::Relaxed);
}

/// Whether free frames are short (under 1/16 of all of them): caches then
/// give back their clean pages.
pub fn under_pressure() -> bool {
    let stats = mem::frame_stats();
    stats.free < stats.total / 16
}
