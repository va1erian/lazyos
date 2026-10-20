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
//! volume, except for giving clean pages back, which needs no I/O). Every hold
//! of the gate ends by scheduling a commit [`INTERVAL_TICKS`] after the volume
//! first went dirty ([`Ext2::end_hold`], no I/O), and once one is due, the
//! next user task to *return from a syscall* commits it
//! (`fs::flusher::deferred_service`, then [`Ext2::writeback_deferred`]):
//! with no mount table held (other volumes are never waited for), with the
//! shallow stack of a syscall's end, and taking the volume's gate only if it is
//! free. One sorted, coalesced commit every few seconds replaces a round trip
//! per metadata block, and a file written and deleted in between never reaches
//! the stick at all. `fsync`, `sync` and the power path commit as on every
//! volume; a writer that fills the dirty limit writes back in its own context.
//!
//! Who commits: never the kernel task, never a task that serves a provider
//! disk (`usbd` waiting on its own disk would never return, issue #704), never
//! one that may not park; an interactive or realtime task (a compositor, the
//! shell's UI) only once the commit is a whole interval late, so a busy
//! desktop's frames do not pay for it while anything else runs. Syscalls are
//! made all the time, so the bound holds whether or not anyone reads `/home`.
//!
//! A stick pulled out with dirty blocks loses what a crash loses: the last
//! few seconds, in the commit's crash-safe order. Once its disk is dead the
//! volume refuses every change ([`Ext2::refuse_dead`]), as it did uncached.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::Ext2;
use crate::fs::flusher::INTERVAL_TICKS;
use crate::fs::vfs::FsError;
use crate::task::{self, PriorityClass};

/// `due` while the volume needs no writeback.
pub const CLEAN: u64 = u64::MAX;
/// Ticks before a commit that found the gate busy is tried again.
const RETRY_TICKS: u64 = 10;

/// The earliest commit due over every deferred volume ([`CLEAN`]: none): the
/// one load a syscall's end pays when nothing is due.
static NEXT_DUE: AtomicU64 = AtomicU64::new(CLEAN);

/// When the deferred writeback of one volume is due.
pub(super) struct Deferred {
    /// The tick at which the dirty set is old enough, or [`CLEAN`].
    due: AtomicU64,
    /// Whether the disk took writes at mount: one that stops is dead.
    writable: bool,
}

impl Deferred {
    pub(super) fn new(writable: bool) -> Deferred {
        Deferred {
            due: AtomicU64::new(CLEAN),
            writable,
        }
    }
}

/// The schedule after a hold: [`CLEAN`] with nothing to write, else the
/// standing `due`, or one interval from `now` for a newly dirty volume.
pub fn schedule(needs_writeback: bool, due: u64, now: u64) -> u64 {
    match (needs_writeback, due) {
        (false, _) => CLEAN,
        (true, CLEAN) => now.saturating_add(INTERVAL_TICKS),
        (true, due) => due,
    }
}

/// When a holder of this kind commits a writeback due at `due`: at once, or
/// a whole interval late for an interactive one; never for one that may not.
pub fn writer_due(due: u64, may_write: bool, interactive: bool) -> Option<u64> {
    if !may_write || due == CLEAN {
        return None;
    }
    Some(if interactive {
        due.saturating_add(INTERVAL_TICKS)
    } else {
        due
    })
}

/// Whether the current task may write a provider disk back: not the kernel
/// task, not a provider, and able to park.
pub fn may_write_here() -> bool {
    let me = task::current();
    me != task::KERNEL_TASK && !crate::block::provider::serves_disk(me) && task::relax::can_block()
}

/// Whether the current task's commits wait an extra interval.
fn interactive_here() -> bool {
    matches!(
        task::priority(task::current()),
        Some(PriorityClass::Interactive | PriorityClass::Realtime)
    )
}

/// At a syscall's end: whether the current task takes the due commits at
/// `now` (it then commits every due volume, each scheduling itself again if
/// it still needs to). Nothing due costs one load and a compare.
pub fn claim(now: u64) -> bool {
    let due = NEXT_DUE.load(Ordering::Relaxed);
    if now < due {
        return false;
    }
    match writer_due(due, may_write_here(), interactive_here()) {
        Some(at) if now >= at => {}
        _ => return false,
    }
    NEXT_DUE
        .compare_exchange(due, CLEAN, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

/// A volume's commit is due at `at`.
pub fn note_due(at: u64) {
    NEXT_DUE.fetch_min(at, Ordering::Relaxed);
}

impl Ext2 {
    /// The end of a hold of the gate (still held): schedule a deferred
    /// volume's commit. No I/O here: the gate may be held inside a mount table.
    pub(super) fn end_hold(&self) {
        let Some(deferred) = &self.deferred else {
            return;
        };
        let due = schedule(
            self.volume.needs_writeback(),
            deferred.due.load(Ordering::Relaxed),
            task::ticks(),
        );
        deferred.due.store(due, Ordering::Relaxed);
        if due != CLEAN {
            note_due(due);
        }
    }

    /// Commit this volume if its writeback is due and the current task may
    /// (`fs::flusher::deferred_service`, at a syscall's end). Never waits for
    /// the gate: a busy volume is tried again shortly.
    pub fn writeback_deferred(&self) {
        let Some(deferred) = &self.deferred else {
            return;
        };
        let now = task::ticks();
        let due = deferred.due.load(Ordering::Relaxed);
        match writer_due(due, may_write_here(), interactive_here()) {
            Some(at) if now >= at => {}
            _ => {
                if due != CLEAN {
                    note_due(due);
                }
                return;
            }
        }
        let Some(_gate) = self.try_enter() else {
            note_due(now.saturating_add(RETRY_TICKS));
            return;
        };
        if self.volume.writeback().is_err() {
            // Recorded for the next `sync` to report; tried again an interval
            // later, not at every syscall. The hold's end keeps this schedule.
            deferred
                .due
                .store(now.saturating_add(INTERVAL_TICKS), Ordering::Relaxed);
            log_failure(self.device);
        }
    }

    /// Whether the kernel task's flusher leaves this volume's writeback to
    /// its users.
    pub(super) fn defers_writeback(&self) -> bool {
        self.deferred.is_some()
    }

    /// A change to a deferred volume whose disk died (pulled out, its
    /// provider gone) is refused, as it was uncached: the cache would take it
    /// and lose it. Reads still answer from the cache.
    pub(super) fn refuse_dead(&self) -> Result<(), FsError> {
        match &self.deferred {
            Some(deferred) if deferred.writable && !self.io.device.is_writable() => {
                Err(FsError::Invalid)
            }
            _ => Ok(()),
        }
    }

    /// Make the deferred writeback due now (tests: no 5 s wait).
    #[cfg(lazyos_tests)]
    pub fn make_writeback_due(&self) {
        if let Some(deferred) = &self.deferred {
            deferred.due.store(task::ticks(), Ordering::Relaxed);
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
