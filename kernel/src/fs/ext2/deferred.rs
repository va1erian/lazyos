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
//! volume, except for giving clean pages back, which needs no I/O, and
//! `flush_page` pushes no mapped page into one). Every hold of the gate ends
//! by scheduling a commit [`INTERVAL_TICKS`] after the volume first went dirty
//! ([`Ext2::end_hold`], no I/O), and once one is due, the next user task to
//! *return from a syscall* writes the mapped pages and commits it
//! (`fs::flusher::deferred_service`, then [`Ext2::writeback_deferred`]): with
//! no mount table held, with the shallow stack of a syscall's end, and taking
//! the volume's gate only if it is free. A path syscall that meets the gate
//! meanwhile still waits holding its mount table, so each commit says how long
//! it held the gate (`FS:DEFERRED`). One sorted, coalesced commit every few
//! seconds replaces a round trip
//! per metadata block, and a file written and deleted in between never reaches
//! the stick at all.
//!
//! A commit is written in *pieces* (`ext2fs::Ext2::writeback_piece`): one that
//! wrote 685 blocks held the gate for 1.37 s on a real PC. Each call takes the
//! gate for at most [`PIECE_NS`] or [`PIECE_REQUESTS`] requests and lets go;
//! the commit stays due, so the next syscall's end of a task that may write
//! continues it, and the device flush after the last piece is made outside
//! the gate. `FS:DEFERRED` is per piece (`blocks` dirty at its start, `us` the
//! gate hold of that piece, `more` whether the commit goes on). Changes made
//! between pieces are not lost or half written: the pieces write the dirty
//! set as it was when the commit began, and later changes wait for the next
//! commit. `fsync`, `sync` and the power path commit as on every
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

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::Ext2;
use crate::fs::flusher::INTERVAL_TICKS;
use crate::fs::vfs::FsError;
use crate::task::{self, PriorityClass};

/// `due` while the volume needs no writeback.
pub const CLEAN: u64 = u64::MAX;
/// Ticks before a commit that found the gate busy is tried again.
const RETRY_TICKS: u64 = 10;
/// One piece of a commit ends after this many requests or this long (ns),
/// whichever comes first: the longest a commit holds the gate at a stretch,
/// besides one request.
static PIECE_REQUESTS: AtomicU32 = AtomicU32::new(32);
const PIECE_NS: u64 = 25_000_000;

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
/// task, not one about to die (its commit would delay its end), able to
/// park, and not a provider. Cheapest tests first; `serves_disk` takes the
/// disk table's lock.
pub fn may_write_here() -> bool {
    let me = task::current();
    me != task::KERNEL_TASK
        && !task::signal::fatal_pending()
        && task::relax::can_block()
        && !crate::block::provider::serves_disk(me)
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
/// it still needs to). Nothing due costs one load and a compare; a due
/// commit costs the class lookup, then the writer checks, until claimed.
pub fn claim(now: u64) -> bool {
    let due = NEXT_DUE.load(Ordering::Relaxed);
    if now < due {
        return false;
    }
    match writer_due(due, true, interactive_here()) {
        Some(at) if now >= at && may_write_here() => {}
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
        let Some(gate) = self.try_enter() else {
            note_due(now.saturating_add(RETRY_TICKS));
            return;
        };
        // One bounded piece per call (see the module docs): the gate is held
        // for the piece alone, and the commit goes on at the next syscall's
        // end of a task that may, which is long enough for a waiter to take
        // the gate (it hands off to its waiters, `task::yieldlock`).
        let blocks = self.volume.dirty_blocks();
        let start = crate::arch::clock::monotonic_ns();
        let mut requests = 0u32;
        let limit = PIECE_REQUESTS.load(Ordering::Relaxed);
        let result = self.volume.writeback_piece(&mut || {
            requests += 1;
            requests >= limit || crate::arch::clock::monotonic_ns() - start >= PIECE_NS
        });
        let us = (crate::arch::clock::monotonic_ns() - start) / 1000;
        let more = matches!(result, Ok(ext2fs::Progress::More));
        crate::trace::event!(
            tracefmt::Channel::Fs,
            "DEFERRED",
            device = self.device,
            blocks = blocks,
            us = us,
            more = more,
            ok = result.is_ok()
        );
        if result.is_err() {
            // Recorded for the next `sync` to report; tried again an interval
            // later, not at every syscall. The hold's end keeps this schedule.
            deferred
                .due
                .store(now.saturating_add(INTERVAL_TICKS), Ordering::Relaxed);
            log_failure(self.device);
        }
        drop(gate);
        // The commit is on the device only after its flush, which no other
        // user of the volume waits for.
        if matches!(result, Ok(ext2fs::Progress::Done)) && self.flush_device().is_err() {
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

    /// Whether no task holds the gate right now (tests: what an `fsync`'s
    /// device flush leaves free).
    #[cfg(lazyos_tests)]
    pub fn gate_is_free(&self) -> bool {
        self.gate.try_lock().is_some()
    }

    /// Whether a commit has been written in part and goes on at the next
    /// call of [`Ext2::writeback_deferred`] (tests).
    #[cfg(lazyos_tests)]
    pub fn commit_in_progress(&self) -> bool {
        self.volume.commit_in_progress()
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

/// Tests: requests per piece of a deferred commit (default 32).
#[cfg(lazyos_tests)]
pub fn set_piece_requests(requests: u32) {
    PIECE_REQUESTS.store(requests.max(1), Ordering::Relaxed);
}
