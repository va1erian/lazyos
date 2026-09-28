//! Anonymous pipes and minimal `AF_UNIX` socket pairs (issue #135).
//!
//! `std::process::Command` on static musl builds its stdio wiring from
//! `pipe2(O_CLOEXEC)` (with a `pipe` fallback) plus an
//! `AF_UNIX`/`SOCK_SEQPACKET` `socketpair` for the exec-error channel, so the
//! Linux shim needs a real byte-stream object with the POSIX blocking rules.
//!
//! Each [`Pipe`] is one-way: a bounded ring with independent reader and writer
//! refcounts. A read end sleeps on `read_wq` and wakes when a writer stores
//! bytes or closes its last writer. A write end sleeps on `write_wq` and wakes
//! when a reader drains bytes or closes its last reader. The rules match Linux:
//!
//! * a read on an empty pipe whose writer count is zero returns end-of-file
//!   (`Ok(0)`);
//! * a write with no readers returns [`Error::BrokenPipe`] (`-EPIPE`).
//!
//! **SIGPIPE is not delivered.** Rust's std ignores SIGPIPE by default (and
//! only resets it to `SIG_DFL` in a spawned child), and the signal layer has no
//! `SIGPIPE`-from-kernel path yet; returning `-EPIPE` is the documented
//! behaviour, matching a process that ignores SIGPIPE. Delivering it would
//! require the writer's process group and a signal-on-syscall path, which is
//! deferred with the rest of the terminal job-control work.
//!
//! Capacity is [`CAPACITY`] (64 KiB, Linux's default pipe size) and the number
//! of live one-way pipes is capped at [`MAX_PIPES`], so the kernel heap cannot
//! be exhausted by pipe creation (a socket pair consumes two).
//!
//! Blocking calls park the current task on a [`WaitQueue`]. As everywhere in
//! the shim, the caller runs with interrupts disabled inside the syscall gate,
//! so the "check condition, register, block" sequence cannot race a notifier on
//! the single CPU.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use crate::task::wait::WaitQueue;
use crate::task::{WaitKind, WakeReason};

/// Bytes a pipe buffers before writers block (Linux's default, 64 KiB).
pub const CAPACITY: usize = 64 * 1024;
/// Maximum live one-way pipes; a `socketpair` holds two.
pub const MAX_PIPES: usize = 64;

/// Live one-way pipes (the ring is allocated eagerly, so this bounds memory).
static LIVE_PIPES: AtomicUsize = AtomicUsize::new(0);

/// Which end of a pipe a descriptor carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Read,
    Write,
}

/// Which end of a [`SocketPair`] a descriptor carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    A,
    B,
}

/// Why a stream I/O call did not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The pipe had no data (read) or no space (write) and is non-blocking.
    WouldBlock,
    /// A write with no readers left (`-EPIPE`, no SIGPIPE; see module docs).
    BrokenPipe,
    /// The read end of a write-only descriptor (or vice versa).
    BadEnd,
    /// The waiting task was interrupted by a deliverable signal (`-EINTR`).
    Interrupted,
}

/// `POLL*` bits (Linux values).
pub const POLLIN: u16 = 0x0001;
pub const POLLOUT: u16 = 0x0004;
pub const POLLERR: u16 = 0x0008;
pub const POLLHUP: u16 = 0x0010;

/// The byte ring. `len` bytes starting at `head` are valid; the free space is
/// the rest, so the buffer wraps without a growable deque.
struct Ring {
    buf: Vec<u8>,
    head: usize,
    len: usize,
}

impl Ring {
    fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copy the oldest `min(len, dst.len())` bytes out, advancing the head.
    fn drain_into(&mut self, dst: &mut [u8]) -> usize {
        let n = self.len.min(dst.len());
        let first = (self.buf.len() - self.head).min(n);
        dst[..first].copy_from_slice(&self.buf[self.head..self.head + first]);
        if n > first {
            dst[first..n].copy_from_slice(&self.buf[..n - first]);
        }
        self.head = (self.head + n) % self.buf.len();
        self.len -= n;
        n
    }

    /// Copy `min(src.len(), free space)` bytes in after the current tail.
    fn fill_from(&mut self, src: &[u8]) -> usize {
        let n = src.len().min(self.buf.len() - self.len);
        let tail = (self.head + self.len) % self.buf.len();
        let first = (self.buf.len() - tail).min(n);
        self.buf[tail..tail + first].copy_from_slice(&src[..first]);
        if n > first {
            self.buf[..n - first].copy_from_slice(&src[first..n]);
        }
        self.len += n;
        n
    }
}

/// A one-way byte pipe: readers at one end, writers at the other.
pub struct Pipe {
    state: Mutex<Ring>,
    /// Readers parked on an empty pipe.
    read_wq: WaitQueue,
    /// Writers parked on a full pipe.
    write_wq: WaitQueue,
    /// Open read ends; zero means "no readers" (`-EPIPE` for writers).
    readers: AtomicUsize,
    /// Open write ends; zero means "writer closed" (EOF for readers).
    writers: AtomicUsize,
    /// `O_NONBLOCK` of the read end's open file description.
    read_nonblock: AtomicBool,
    /// `O_NONBLOCK` of the write end's open file description.
    write_nonblock: AtomicBool,
}

impl Pipe {
    /// Allocate a pipe with a zeroed ring, or `None` at the live-pipe cap or on
    /// kernel-heap exhaustion.
    pub fn new() -> Option<Arc<Pipe>> {
        LIVE_PIPES
            .try_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                (live < MAX_PIPES).then_some(live + 1)
            })
            .ok()?;
        let mut buf = Vec::new();
        if buf.try_reserve_exact(CAPACITY).is_err() {
            LIVE_PIPES.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        buf.resize(CAPACITY, 0);
        Some(Arc::new(Pipe {
            state: Mutex::new(Ring {
                buf,
                head: 0,
                len: 0,
            }),
            read_wq: WaitQueue::new(WaitKind::Pipe),
            write_wq: WaitQueue::new(WaitKind::Pipe),
            readers: AtomicUsize::new(0),
            writers: AtomicUsize::new(0),
            read_nonblock: AtomicBool::new(false),
            write_nonblock: AtomicBool::new(false),
        }))
    }

    /// Number of live one-way pipes (test/soak observable).
    pub fn live() -> usize {
        LIVE_PIPES.load(Ordering::Acquire)
    }

    /// Take one reference on `end` (a new descriptor or a `dup`).
    pub fn acquire(&self, end: End) {
        let counter = match end {
            End::Read => &self.readers,
            End::Write => &self.writers,
        };
        counter.fetch_add(1, Ordering::AcqRel);
    }

    /// Drop one reference on `end`. The last writer wakes readers with EOF;
    /// the last reader wakes writers with `-EPIPE`; either can make `poll` on
    /// the other end newly interesting.
    pub fn release(&self, end: End) {
        let remaining = match end {
            End::Read => self.readers.fetch_sub(1, Ordering::AcqRel) - 1,
            End::Write => self.writers.fetch_sub(1, Ordering::AcqRel) - 1,
        };
        if remaining == 0 {
            match end {
                End::Read => {
                    self.write_wq.notify_all();
                }
                End::Write => {
                    self.read_wq.notify_all();
                }
            }
            crate::task::notify_poll();
        }
    }

    /// Reader count (observable for tests).
    pub fn readers(&self) -> usize {
        self.readers.load(Ordering::Acquire)
    }

    /// Writer count (observable for tests).
    pub fn writers(&self) -> usize {
        self.writers.load(Ordering::Acquire)
    }

    /// Whether the read end would make progress (data or EOF).
    pub fn readable(&self) -> bool {
        let ring = self.state.lock();
        !ring.is_empty() || self.writers.load(Ordering::Acquire) == 0
    }

    /// Whether the write end would make progress (space and a reader).
    pub fn writable(&self) -> bool {
        let ring = self.state.lock();
        ring.len < ring.buf.len() && self.readers.load(Ordering::Acquire) > 0
    }

    /// `O_NONBLOCK` state of one end's open file description.
    pub fn nonblock(&self, end: End) -> bool {
        match end {
            End::Read => self.read_nonblock.load(Ordering::Acquire),
            End::Write => self.write_nonblock.load(Ordering::Acquire),
        }
    }

    /// Set `O_NONBLOCK` on one end. `dup`/`fork` share this state, as both
    /// share the open file description.
    pub fn set_nonblock(&self, end: End, on: bool) {
        match end {
            End::Read => self.read_nonblock.store(on, Ordering::Release),
            End::Write => self.write_nonblock.store(on, Ordering::Release),
        }
    }

    /// Read up to `dst.len()` bytes. `Ok(0)` is end-of-file (all writers
    /// closed); a non-blocking empty pipe is [`Error::WouldBlock`].
    pub fn read(&self, end: End, dst: &mut [u8], nonblock: bool) -> Result<usize, Error> {
        if end != End::Read {
            return Err(Error::BadEnd);
        }
        if dst.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut ring = self.state.lock();
                if !ring.is_empty() {
                    let n = ring.drain_into(dst);
                    drop(ring);
                    self.write_wq.notify_all();
                    crate::task::notify_poll();
                    return Ok(n);
                }
                if self.writers.load(Ordering::Acquire) == 0 {
                    return Ok(0); // EOF
                }
            }
            if nonblock {
                return Err(Error::WouldBlock);
            }
            match self.read_wq.wait(crate::task::current(), None) {
                WakeReason::Interrupted => return Err(Error::Interrupted),
                WakeReason::Woken | WakeReason::TimedOut => {}
            }
        }
    }

    /// Write `src`, returning how many bytes were accepted (a short write is
    /// legal on a pipe; callers that need all of it retry). Blocking waits for
    /// space; a writer with no readers is [`Error::BrokenPipe`].
    pub fn write(&self, src: &[u8], end: End, nonblock: bool) -> Result<usize, Error> {
        if end != End::Write {
            return Err(Error::BadEnd);
        }
        if src.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut ring = self.state.lock();
                if self.readers.load(Ordering::Acquire) == 0 {
                    return Err(Error::BrokenPipe);
                }
                if ring.len < ring.buf.len() {
                    let n = ring.fill_from(src);
                    drop(ring);
                    self.read_wq.notify_all();
                    crate::task::notify_poll();
                    return Ok(n);
                }
            }
            if nonblock {
                return Err(Error::WouldBlock);
            }
            match self.write_wq.wait(crate::task::current(), None) {
                WakeReason::Interrupted => return Err(Error::Interrupted),
                WakeReason::Woken | WakeReason::TimedOut => {}
            }
        }
    }

    /// `poll` revents for one end: `POLLIN`/`POLLOUT` when the requested event
    /// can proceed, `POLLHUP` when the read end has no writers left, `POLLERR`
    /// when the write end has no readers left.
    pub fn poll(&self, end: End, events: u16) -> u16 {
        let ring = self.state.lock();
        let mut revents = 0;
        match end {
            End::Read => {
                if events & POLLIN != 0 && !ring.is_empty() {
                    revents |= POLLIN;
                }
                if self.writers.load(Ordering::Acquire) == 0 {
                    revents |= POLLHUP;
                }
            }
            End::Write => {
                if events & POLLOUT != 0
                    && ring.len < ring.buf.len()
                    && self.readers.load(Ordering::Acquire) > 0
                {
                    revents |= POLLOUT;
                }
                if self.readers.load(Ordering::Acquire) == 0 {
                    revents |= POLLERR;
                }
            }
        }
        revents
    }

    /// Park the current task on the reader queue (test hook). Production reads
    /// go through [`Pipe::read`], which uses the same queue.
    #[cfg(laZYOS_TESTS)]
    pub fn park_reader(&self, task: usize) {
        self.read_wq.park(task, None);
    }

    /// Park the current task on the writer queue (test hook).
    #[cfg(laZYOS_TESTS)]
    pub fn park_writer(&self, task: usize) {
        self.write_wq.park(task, None);
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        LIVE_PIPES.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A pair of cross-connected pipes: each side reads what the other writes.
///
/// Linux's `SOCK_SEQPACKET` preserves message boundaries; this byte-stream
/// approximation does not, which is enough for the std shim (it sends one
/// fixed-size 8-byte record and the parent reads exactly that). `SOCK_STREAM`
/// callers cannot observe the difference. Documented in `docs/compat` terms:
/// `recvmsg` message framing is not implemented.
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
}

impl SocketPair {
    /// Build both directions, or `None` when the pipe cap is reached.
    pub fn new() -> Option<Arc<SocketPair>> {
        let ab = Pipe::new()?;
        let ba = Pipe::new()?;
        Some(Arc::new(SocketPair {
            ab,
            ba,
            open: [AtomicUsize::new(0), AtomicUsize::new(0)],
            nonblock: [AtomicBool::new(false), AtomicBool::new(false)],
        }))
    }

    fn index(side: Side) -> usize {
        match side {
            Side::A => 0,
            Side::B => 1,
        }
    }

    /// The (read-from-peer, write-to-peer) pipe pair for `side`.
    fn directions(&self, side: Side) -> (&Pipe, &Pipe) {
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
    /// side's direction pipes (EOF/`-EPIPE` for the peer).
    pub fn close(&self, side: Side) {
        let index = Self::index(side);
        if self.open[index].fetch_sub(1, Ordering::AcqRel) == 1 {
            let (read, write) = self.directions(side);
            read.release(End::Read);
            write.release(End::Write);
        }
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

    /// Read bytes the peer wrote; `Ok(0)` once the peer side is fully closed.
    pub fn read(&self, side: Side, dst: &mut [u8], nonblock: bool) -> Result<usize, Error> {
        let (read, _) = self.directions(side);
        read.read(End::Read, dst, nonblock)
    }

    /// Write bytes for the peer to read.
    pub fn write(&self, side: Side, src: &[u8], nonblock: bool) -> Result<usize, Error> {
        let (_, write) = self.directions(side);
        write.write(src, End::Write, nonblock)
    }

    /// `poll` revents for one side, merging its read and write directions.
    pub fn poll(&self, side: Side, events: u16) -> u16 {
        let (read, write) = self.directions(side);
        read.poll(End::Read, events) | write.poll(End::Write, events)
    }
}
