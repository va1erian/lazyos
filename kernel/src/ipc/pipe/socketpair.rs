//! [`SocketPair`]: two cross-connected [`Pipe`]s behind `socketpair(2)`.

use super::*;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

/// A pair of cross-connected pipes: each side reads what the other writes.
///
/// Both `SOCK_STREAM` (byte stream) and `SOCK_SEQPACKET` (message boundaries,
/// truncation on small reads) pairs are built from the same structure; the
/// direction pipes carry the framing. `shutdown` releases one direction while
/// the descriptor stays open, and the per-side bits keep the later `close`
/// from releasing it twice.
pub struct SocketPair {
    /// Bytes written by side A, read by side B.
    ab: Arc<Pipe>,
    /// Bytes written by side B, read by side A.
    ba: Arc<Pipe>,
    /// Open descriptors per side (`dup`/`fork` share); the last close releases
    /// both direction references of that side.
    open: [AtomicUsize; 2],
    /// `O_NONBLOCK` of each side's socket (one open file description per side).
    nonblock: [AtomicBool; 2],
    /// Shut directions per side: bit 0 = `SHUT_RD`, bit 1 = `SHUT_WR`.
    shut: [AtomicU8; 2],
}

/// `shutdown(2)` direction bits.
const SHUT_RD: u8 = 1;
const SHUT_WR: u8 = 2;

impl SocketPair {
    /// Build a byte-stream (`SOCK_STREAM`) pair.
    pub fn new() -> Option<Arc<SocketPair>> {
        Self::new_with(Mode::Stream)
    }

    /// Build a message-preserving (`SOCK_SEQPACKET`) pair.
    pub fn new_seqpacket() -> Option<Arc<SocketPair>> {
        Self::new_with(Mode::Seqpacket)
    }

    /// Build both directions with the given framing, or `None` when the pipe
    /// cap is reached.
    pub fn new_with(mode: Mode) -> Option<Arc<SocketPair>> {
        let ab = Pipe::new_with(mode)?;
        let ba = Pipe::new_with(mode)?;
        Some(Self::from_pipes(ab, ba))
    }

    /// Build a pair of small-ring pipes (an `AF_INET` socket's data path), or
    /// `None` at the small-ring cap.
    pub fn new_small(mode: Mode) -> Option<Arc<SocketPair>> {
        let ab = Pipe::new_small(mode)?;
        let ba = Pipe::new_small(mode)?;
        Some(Self::from_pipes(ab, ba))
    }

    fn from_pipes(ab: Arc<Pipe>, ba: Arc<Pipe>) -> Arc<SocketPair> {
        Arc::new(SocketPair {
            ab,
            ba,
            open: [AtomicUsize::new(0), AtomicUsize::new(0)],
            nonblock: [AtomicBool::new(false), AtomicBool::new(false)],
            shut: [AtomicU8::new(0), AtomicU8::new(0)],
        })
    }

    /// This pair's framing.
    pub fn mode(&self) -> Mode {
        self.ab.mode()
    }

    /// Whether this pair preserves message boundaries.
    pub fn seqpacket(&self) -> bool {
        self.mode() == Mode::Seqpacket
    }

    fn index(side: Side) -> usize {
        match side {
            Side::A => 0,
            Side::B => 1,
        }
    }

    /// The (read-from-peer, write-to-peer) pipe pair for `side`.
    pub(super) fn directions(&self, side: Side) -> (&Pipe, &Pipe) {
        match side {
            Side::A => (&self.ba, &self.ab),
            Side::B => (&self.ab, &self.ba),
        }
    }

    /// Take one descriptor reference on `side`; the first one also takes the
    /// side's references on both direction pipes.
    pub fn acquire(&self, side: Side) {
        let index = Self::index(side);
        if self.open[index].fetch_add(1, Ordering::AcqRel) == 0 {
            let (read, write) = self.directions(side);
            read.acquire(End::Read);
            write.acquire(End::Write);
        }
    }

    /// Drop one descriptor reference on `side`; the last one releases the
    /// direction pipes not already shut down (EOF/`-EPIPE` for the peer).
    pub fn close(&self, side: Side) {
        let index = Self::index(side);
        if self.open[index].fetch_sub(1, Ordering::AcqRel) == 1 {
            let shut = self.shut[index].load(Ordering::Acquire);
            let (read, write) = self.directions(side);
            if shut & SHUT_RD == 0 {
                read.release(End::Read);
            }
            if shut & SHUT_WR == 0 {
                write.release(End::Write);
            }
        }
    }

    /// Apply `shutdown(fd, how)` to one side: `0` = read, `1` = write,
    /// `2` = both. Returns false for an unknown direction. Each direction is
    /// released at most once; `close` skips what shutdown already released.
    pub fn shutdown(&self, side: Side, how: u64) -> bool {
        let bits = match how {
            0 => SHUT_RD,
            1 => SHUT_WR,
            2 => SHUT_RD | SHUT_WR,
            _ => return false,
        };
        let index = Self::index(side);
        let added = bits & !self.shut[index].fetch_or(bits, Ordering::AcqRel);
        let (read, write) = self.directions(side);
        if added & SHUT_RD != 0 {
            read.release(End::Read);
        }
        if added & SHUT_WR != 0 {
            write.release(End::Write);
        }
        true
    }

    /// Whether one direction has been shut down (does not consume it).
    pub fn is_shutdown(&self, side: Side, how: u64) -> bool {
        let bit = match how {
            0 => SHUT_RD,
            1 => SHUT_WR,
            _ => return false,
        };
        self.shut[Self::index(side)].load(Ordering::Acquire) & bit != 0
    }

    /// Open descriptor count of one side (observable for tests).
    pub fn open_count(&self, side: Side) -> usize {
        self.open[Self::index(side)].load(Ordering::Acquire)
    }

    /// `O_NONBLOCK` of one side's socket.
    pub fn nonblock(&self, side: Side) -> bool {
        self.nonblock[Self::index(side)].load(Ordering::Acquire)
    }

    /// Set `O_NONBLOCK` on one side's socket.
    pub fn set_nonblock(&self, side: Side, on: bool) {
        self.nonblock[Self::index(side)].store(on, Ordering::Release);
    }

    /// Read bytes the peer wrote; `Ok(0)` once the peer side is fully closed
    /// or this side has been `SHUT_RD`.
    pub fn read(&self, side: Side, dst: &mut [u8], nonblock: bool) -> Result<usize, Error> {
        self.read_until(side, dst, nonblock, None)
    }

    /// [`SocketPair::read`] that waits no later than `deadline` (absolute
    /// ticks), then reports [`Error::WouldBlock`] (`SO_RCVTIMEO`).
    pub fn read_until(
        &self,
        side: Side,
        dst: &mut [u8],
        nonblock: bool,
        deadline: Option<u64>,
    ) -> Result<usize, Error> {
        if self.is_shutdown(side, 0) {
            return Ok(0);
        }
        let (read, _) = self.directions(side);
        read.read_until(End::Read, dst, nonblock, deadline)
    }

    /// Write bytes for the peer to read; `SHUT_WR` makes this `BrokenPipe`.
    pub fn write(&self, side: Side, src: &[u8], nonblock: bool) -> Result<usize, Error> {
        self.write_until(side, src, nonblock, None)
    }

    /// [`SocketPair::write`] that waits for space no later than `deadline`
    /// (absolute ticks), then reports [`Error::WouldBlock`] (`SO_SNDTIMEO`).
    pub fn write_until(
        &self,
        side: Side,
        src: &[u8],
        nonblock: bool,
        deadline: Option<u64>,
    ) -> Result<usize, Error> {
        if self.is_shutdown(side, 1) {
            return Err(Error::BrokenPipe);
        }
        let (_, write) = self.directions(side);
        write.write_until(src, End::Write, nonblock, deadline)
    }

    /// `poll` revents for one side, merging its read and write directions.
    pub fn poll(&self, side: Side, events: u16) -> u16 {
        let (read, write) = self.directions(side);
        read.poll(End::Read, events) | write.poll(End::Write, events)
    }

    /// [`poll`](SocketPair::poll) plus a freshness counter for edge-triggered
    /// `epoll` interests: the counter changes whenever either direction's
    /// readiness could have changed (data, space, or a close).
    pub fn poll_gen(&self, side: Side, events: u16) -> (u16, u64) {
        let (read, write) = self.directions(side);
        let revents = read.poll(End::Read, events) | write.poll(End::Write, events);
        let gen = read.read_events() ^ write.write_events();
        (revents, gen)
    }
}
