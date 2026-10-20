//! Write-back for a volume on a provider disk (a USB stick served by `usbd`),
//! issue #940.
//!
//! Such a volume used to be opened uncached: the periodic flusher runs on the
//! kernel task, which must never wait for `usbd`, so every write went to the
//! stick in the writer's own context. But each write is several requests to a
//! polled user-space driver (the data, then the inode, the bitmaps, the
//! directory, the superblock counters), and the writer holds the volume's gate
//! the whole time, so every other reader and writer of `/home` waited for the
//! stick: LazyWeb+'s cache writes froze its network process (and so the page)
//! for 0.5-2.7 s on a real PC.
//!
//! So the volume is cached like any other, and only *who writes it back*
//! differs. The kernel task never does ([`Ext2::writeback`] skips a deferred
//! volume, except for giving clean pages back, which needs no I/O). Instead the
//! task that ends a hold of the gate ([`Ext2::end_hold`]) writes back once the
//! dirty set is [`INTERVAL_TICKS`] old, in its own context, where it may park:
//! one sorted, coalesced commit every few seconds instead of a round trip per
//! metadata block, and a file written and deleted in between never reaches the
//! stick at all. `fsync`, `sync` and the power path commit as on every volume;
//! a writer that fills the dirty limit writes back in its own context too.
//!
//! The holder never writes back when it is the kernel task, or when it serves
//! a provider disk itself (`usbd` waiting on its own disk would never return,
//! issue #704), or when it may not park.
//!
//! What a stick loses when it is pulled out with dirty blocks is what any
//! cached volume loses at a crash: at most the last few seconds, in the
//! cache's crash-safe order. A volume nobody touches keeps its dirty blocks
//! until the next access or `sync` (the desktop shell reads `/home` every
//! second, and the power path syncs).

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::Ext2;
use crate::fs::flusher::INTERVAL_TICKS;
use crate::task;

/// `due` while the volume has nothing dirty.
const CLEAN: u64 = u64::MAX;

/// When the deferred writeback of one volume is due.
pub(super) struct Deferred {
    /// The tick at which the dirty set is old enough, or [`CLEAN`].
    due: AtomicU64,
}

impl Deferred {
    pub(super) fn new() -> Deferred {
        Deferred {
            due: AtomicU64::new(CLEAN),
        }
    }
}

/// What the end of a gate hold does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing is dirty: forget any schedule.
    Clean,
    /// Dirty, not yet due (or this holder may not write): keep or start the
    /// schedule at the given tick.
    Wait(u64),
    /// Dirty and due, and this holder may write: write back now.
    WriteBack,
}

/// The rule, apart from any state: `dirty` blocks, the schedule `due` ([`CLEAN`]
/// when none), the tick `now`, and whether this holder may write back.
pub fn decide(dirty: usize, due: u64, now: u64, may_write: bool) -> Action {
    if dirty == 0 {
        return Action::Clean;
    }
    let due = if due == CLEAN {
        now.saturating_add(INTERVAL_TICKS)
    } else {
        due
    };
    if may_write && now >= due {
        Action::WriteBack
    } else {
        Action::Wait(due)
    }
}

/// Whether the current task may write a provider disk back: not the kernel
/// task, not a provider, and able to park.
pub fn may_write_here() -> bool {
    let me = task::current();
    me != task::KERNEL_TASK && !crate::block::provider::serves_disk(me) && task::relax::can_block()
}

impl Ext2 {
    /// The end of a hold of the gate (still held): a deferred volume writes
    /// back if its dirty set is due and this holder may.
    pub(super) fn end_hold(&self) {
        let Some(deferred) = &self.deferred else {
            return;
        };
        let due = deferred.due.load(Ordering::Relaxed);
        let now = task::ticks();
        match decide(self.volume.dirty_blocks(), due, now, may_write_here()) {
            Action::Clean => deferred.due.store(CLEAN, Ordering::Relaxed),
            Action::Wait(at) => deferred.due.store(at, Ordering::Relaxed),
            Action::WriteBack => {
                // A failure stays recorded for the next `sync` to report; the
                // next try waits a whole interval rather than every call.
                let failed = self.volume.writeback().is_err();
                let next = match self.volume.dirty_blocks() {
                    0 if !failed => CLEAN,
                    _ => now.saturating_add(INTERVAL_TICKS),
                };
                deferred.due.store(next, Ordering::Relaxed);
                if failed {
                    log_failure(self.device);
                }
            }
        }
    }

    /// Whether the kernel task's flusher leaves this volume's writeback to
    /// its users ([`Ext2::end_hold`]).
    pub(super) fn defers_writeback(&self) -> bool {
        self.deferred.is_some()
    }

    /// Make the deferred writeback due now (tests: no 5 s wait).
    #[cfg(lazyos_tests)]
    pub fn make_writeback_due(&self) {
        if let Some(deferred) = &self.deferred {
            deferred.due.store(0, Ordering::Relaxed);
        }
    }
}

/// The first failed deferred writeback, once per boot.
fn log_failure(device: &str) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        serial_println!("ext2: {device}: deferred writeback failed; the next sync reports it");
    }
}
