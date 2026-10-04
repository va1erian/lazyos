//! Small-ring pipes for `AF_INET` sockets.
//!
//! A pipe's ring is allocated eagerly, so the 64 KiB rings of `pipe(2)` are
//! capped at [`super::MAX_PIPES`]. A network socket's rings are counted
//! against their own cap, [`MAX_SMALL_PIPES`] (two per socket, 64 sockets,
//! which is also `netd`'s own table size). Each holds as much as `netd`'s
//! stack keeps per socket and direction (256 KiB, its TCP window), so a bulk
//! transfer never waits on the ring between the application's calls
//! (docs/performance-plan.md P4.3): 128 rings are 32 MiB of a kernel heap
//! that grows on demand (`limits.rs`, half of RAM by default).
//!
//! **Doorbell.** The pump's doorbell (`ipc::inet::bell`) rings for what the
//! application does on its side of a socket and never for what `netd` does on
//! its own: each ring knows which of its ends is the application's
//! ([`BELL_ON_READ`] for the receive direction, [`BELL_ON_WRITE`] for the
//! send direction).

use super::*;

/// Bytes one `AF_INET` ring buffers (the name predates P4.3, when it was
/// smaller than a pipe's).
pub const SMALL_CAPACITY: usize = 256 * 1024;
/// Most small rings alive at once (two per socket).
pub const MAX_SMALL_PIPES: usize = 128;
/// The application reads this ring (`netd` writes it): a read that makes
/// room `netd` may be waiting for, and the reader's close, ring the bell.
pub const BELL_ON_READ: u8 = 1;
/// The application writes this ring (`netd` reads it): a write into an
/// empty ring, and the writer's close or shutdown, ring the bell.
pub const BELL_ON_WRITE: u8 = 2;

/// Live small rings.
pub(super) static LIVE_SMALL: AtomicUsize = AtomicUsize::new(0);

impl Pipe {
    /// Allocate a pipe with a [`SMALL_CAPACITY`] ring that rings the pump's
    /// doorbell as `bell` says, or `None` at the cap or on kernel-heap
    /// exhaustion.
    pub fn new_small(mode: Mode, bell: u8) -> Option<Arc<Pipe>> {
        LIVE_SMALL
            .try_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                (live < MAX_SMALL_PIPES).then_some(live + 1)
            })
            .ok()?;
        let mut buf = Vec::new();
        if buf.try_reserve_exact(SMALL_CAPACITY).is_err() {
            LIVE_SMALL.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        buf.resize(SMALL_CAPACITY, 0);
        let mut pipe = Pipe::with_buffer(mode, buf, true);
        pipe.bell = bell;
        Some(Arc::new(pipe))
    }

    /// Small rings alive (test/soak observable).
    pub fn live_small() -> usize {
        LIVE_SMALL.load(Ordering::Acquire)
    }

    /// The application read from a ring that had `free_before` bytes free:
    /// `netd` can only be holding bytes for it when that was nearly nothing.
    pub(super) fn bell_after_read(&self, free_before: usize) {
        if self.bell & BELL_ON_READ != 0 && free_before < crate::ipc::inet::bell::LOW_SPACE {
            crate::ipc::inet::bell::ring();
        }
    }

    /// The application wrote; `netd` only needs telling when the ring was
    /// empty (otherwise it still has bytes here to come back for).
    pub(super) fn bell_after_write(&self, was_empty: bool) {
        if self.bell & BELL_ON_WRITE != 0 && was_empty {
            crate::ipc::inet::bell::ring();
        }
    }

    /// The application's last reference on its end of the ring went away.
    pub(super) fn bell_on_release(&self, end: End) {
        let mine = match end {
            End::Read => BELL_ON_READ,
            End::Write => BELL_ON_WRITE,
        };
        if self.bell & mine != 0 {
            crate::ipc::inet::bell::ring();
        }
    }
}
