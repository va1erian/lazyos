//! Small-ring pipes for `AF_INET` sockets.
//!
//! The kernel heap is 16 MiB and a pipe's ring is allocated eagerly, so the
//! 64 KiB rings of `pipe(2)` are capped at [`super::MAX_PIPES`]. A network
//! socket needs two directions but only as much buffering as `netd` keeps
//! (16 KiB each way), so these rings are [`SMALL_CAPACITY`] and counted
//! against their own cap, [`MAX_SMALL_PIPES`]: 128 rings are 4 MiB at most,
//! enough for 64 sockets, which is also `netd`'s own table size.

use super::*;

/// Bytes one small ring buffers.
pub const SMALL_CAPACITY: usize = 32 * 1024;
/// Most small rings alive at once (two per socket).
pub const MAX_SMALL_PIPES: usize = 128;

/// Live small rings.
pub(super) static LIVE_SMALL: AtomicUsize = AtomicUsize::new(0);

impl Pipe {
    /// Allocate a pipe with a [`SMALL_CAPACITY`] ring, or `None` at the cap or
    /// on kernel-heap exhaustion.
    pub fn new_small(mode: Mode) -> Option<Arc<Pipe>> {
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
        Some(Arc::new(Pipe::with_buffer(mode, buf, true)))
    }

    /// Small rings alive (test/soak observable).
    pub fn live_small() -> usize {
        LIVE_SMALL.load(Ordering::Acquire)
    }
}
