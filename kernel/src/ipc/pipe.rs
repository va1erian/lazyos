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
//! A write with no reader is [`Error::BrokenPipe`]; the syscall layer turns
//! that `-EPIPE` into `SIGPIPE` for the writer, as Linux does
//! (`process/linux/extra.rs`), so this module stays signal-free.
//!
//! Capacity is [`CAPACITY`] (64 KiB, Linux's default pipe size) and the number
//! of live one-way pipes is capped at [`MAX_PIPES`], so the kernel heap cannot
//! be exhausted by pipe creation (a socket pair consumes two).
//!
//! Blocking calls park the current task on a [`WaitQueue`]. The caller runs
//! with interrupts disabled inside the syscall gate, so "check, register,
//! block" cannot race a notifier on the single CPU. `MSG_PEEK` is `peek.rs`.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use crate::task::wait::WaitQueue;
use crate::task::{WaitKind, WakeReason};

mod peek;
mod poll;
mod small;
mod socketpair;

#[cfg_attr(not(lazyos_tests), allow(unused_imports))] // the tests check the cap
pub use small::{MAX_SMALL_PIPES, SMALL_CAPACITY};
pub use socketpair::SocketPair;

/// Bytes a pipe buffers before writers block (Linux's default, 64 KiB).
pub const CAPACITY: usize = 64 * 1024;
/// Maximum live one-way pipes; a `socketpair` holds two.
pub const MAX_PIPES: usize = 64;
/// Maximum number of messages a `SOCK_SEQPACKET` direction may queue. Bounds
/// the framing bookkeeping when many zero-length messages are sent.
pub const MAX_FRAMES: usize = 1024;

/// Framing of a one-way stream: a byte stream, or discrete messages
/// (`SOCK_SEQPACKET`, where each write is one record).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Stream,
    Seqpacket,
}

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
    /// A `SOCK_SEQPACKET` message larger than the buffer (`-EMSGSIZE`).
    MessageTooLong,
    /// An argument the stream layer rejects (`-EINVAL`).
    Invalid,
}

/// `POLL*` bits (Linux values).
pub const POLLIN: u16 = 0x0001;
pub const POLLOUT: u16 = 0x0004;
pub const POLLERR: u16 = 0x0008;
pub const POLLHUP: u16 = 0x0010;

/// The byte ring. `len` bytes starting at `head` are valid; the free space is
/// the rest, so the buffer wraps without a growable deque. In seqpacket mode
/// `frames` records the byte length of each queued message, oldest first.
struct Ring {
    buf: Vec<u8>,
    head: usize,
    len: usize,
    mode: Mode,
    frames: VecDeque<usize>,
}

impl Ring {
    fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether a read would make progress: any bytes, or (seqpacket) any
    /// message including a zero-length one.
    fn has_data(&self) -> bool {
        self.len > 0 || !self.frames.is_empty()
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

    /// Drop `n` oldest bytes without copying them out (seqpacket truncation).
    fn discard(&mut self, n: usize) {
        debug_assert!(n <= self.len);
        self.head = (self.head + n) % self.buf.len();
        self.len -= n;
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
    /// Events that made the read end fresh (a write, or the last writer
    /// closing). `epoll` edge-triggered interests compare it between waits.
    read_events: AtomicU64,
    /// Events that made the write end fresh (a read, or the last reader
    /// closing), mirroring [`read_events`](Pipe::read_events).
    write_events: AtomicU64,
    /// Allocated from the small-ring budget (`small.rs`), not `MAX_PIPES`.
    small: bool,
}

impl Pipe {
    /// Allocate a byte-stream pipe with a zeroed ring, or `None` at the
    /// live-pipe cap or on kernel-heap exhaustion.
    pub fn new() -> Option<Arc<Pipe>> {
        Self::new_with(Mode::Stream)
    }

    /// Allocate a message-preserving (`SOCK_SEQPACKET`) pipe.
    pub fn new_seqpacket() -> Option<Arc<Pipe>> {
        Self::new_with(Mode::Seqpacket)
    }

    /// Allocate a pipe with the given framing.
    pub fn new_with(mode: Mode) -> Option<Arc<Pipe>> {
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
        Some(Arc::new(Pipe::with_buffer(mode, buf, false)))
    }

    /// A pipe over an allocated ring (the budget has been charged already).
    fn with_buffer(mode: Mode, buf: Vec<u8>, small: bool) -> Pipe {
        Pipe {
            state: Mutex::new(Ring {
                buf,
                head: 0,
                len: 0,
                mode,
                frames: VecDeque::new(),
            }),
            read_wq: WaitQueue::new(WaitKind::Pipe),
            write_wq: WaitQueue::new(WaitKind::Pipe),
            readers: AtomicUsize::new(0),
            writers: AtomicUsize::new(0),
            read_nonblock: AtomicBool::new(false),
            write_nonblock: AtomicBool::new(false),
            read_events: AtomicU64::new(0),
            write_events: AtomicU64::new(0),
            small,
        }
    }

    /// Number of live one-way pipes (test/soak observable).
    pub fn live() -> usize {
        LIVE_PIPES.load(Ordering::Acquire)
    }

    /// This pipe's framing.
    pub fn mode(&self) -> Mode {
        self.state.lock().mode
    }

    /// Count of read-readiness events (writes and last-writer closes).
    pub fn read_events(&self) -> u64 {
        self.read_events.load(Ordering::Acquire)
    }

    /// Count of write-readiness events (reads and last-reader closes).
    pub fn write_events(&self) -> u64 {
        self.write_events.load(Ordering::Acquire)
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
    /// the other end newly interesting (and counts as an edge).
    pub fn release(&self, end: End) {
        let remaining = match end {
            End::Read => self.readers.fetch_sub(1, Ordering::AcqRel) - 1,
            End::Write => self.writers.fetch_sub(1, Ordering::AcqRel) - 1,
        };
        if remaining == 0 {
            match end {
                End::Read => {
                    self.write_events.fetch_add(1, Ordering::AcqRel);
                    self.write_wq.notify_all();
                }
                End::Write => {
                    self.read_events.fetch_add(1, Ordering::AcqRel);
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
        ring.has_data() || self.writers.load(Ordering::Acquire) == 0
    }

    /// Whether the write end would make progress (space and a reader).
    pub fn writable(&self) -> bool {
        let ring = self.state.lock();
        self.space_for(&ring, 1) && self.readers.load(Ordering::Acquire) > 0
    }

    /// Whether `len` bytes fit in the ring (all-or-nothing in seqpacket mode).
    fn space_for(&self, ring: &Ring, len: usize) -> bool {
        match ring.mode {
            Mode::Stream => ring.len < ring.buf.len(),
            Mode::Seqpacket => {
                ring.frames.len() < MAX_FRAMES
                    && len <= ring.buf.len()
                    && ring.len + len <= ring.buf.len()
            }
        }
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
    ///
    /// A seqpacket read returns at most one message. A message longer than
    /// `dst` is truncated: the copied prefix is returned and the rest of the
    /// message is discarded, matching Linux `recv` without `MSG_TRUNC`.
    pub fn read(&self, end: End, dst: &mut [u8], nonblock: bool) -> Result<usize, Error> {
        self.read_until(end, dst, nonblock, None)
    }

    /// [`Pipe::read`] that waits no later than `deadline` (absolute ticks):
    /// past it, an empty pipe is [`Error::WouldBlock`] (`SO_RCVTIMEO`).
    pub fn read_until(
        &self,
        end: End,
        dst: &mut [u8],
        nonblock: bool,
        deadline: Option<u64>,
    ) -> Result<usize, Error> {
        if end != End::Read {
            return Err(Error::BadEnd);
        }
        if dst.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut ring = self.state.lock();
                if ring.mode == Mode::Seqpacket {
                    if let Some(&message) = ring.frames.front() {
                        let n = message.min(dst.len());
                        let copied = ring.drain_into(&mut dst[..n]);
                        if message > copied {
                            ring.discard(message - copied);
                        }
                        ring.frames.pop_front();
                        drop(ring);
                        self.write_events.fetch_add(1, Ordering::AcqRel);
                        self.write_wq.notify_all();
                        crate::task::notify_poll();
                        return Ok(n);
                    }
                } else if !ring.is_empty() {
                    let n = ring.drain_into(dst);
                    drop(ring);
                    self.write_events.fetch_add(1, Ordering::AcqRel);
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
            block_on(&self.read_wq, deadline)?;
        }
    }

    /// Write `src`, returning how many bytes were accepted (a short write is
    /// legal on a pipe; callers that need all of it retry). Blocking waits for
    /// space; a writer with no readers is [`Error::BrokenPipe`].
    ///
    /// A seqpacket write is all-or-nothing: the whole call becomes one message,
    /// or the call blocks/`-EAGAIN`s. A message larger than the ring is
    /// [`Error::MessageTooLong`] (`-EMSGSIZE`).
    pub fn write(&self, src: &[u8], end: End, nonblock: bool) -> Result<usize, Error> {
        self.write_until(src, end, nonblock, None)
    }

    /// [`Pipe::write`] that waits no later than `deadline` (absolute ticks):
    /// past it, a full pipe is [`Error::WouldBlock`] (`SO_SNDTIMEO`).
    pub fn write_until(
        &self,
        src: &[u8],
        end: End,
        nonblock: bool,
        deadline: Option<u64>,
    ) -> Result<usize, Error> {
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
                if ring.mode == Mode::Seqpacket && src.len() > ring.buf.len() {
                    return Err(Error::MessageTooLong);
                }
                if self.space_for(&ring, src.len()) {
                    let n = ring.fill_from(src);
                    if ring.mode == Mode::Seqpacket {
                        ring.frames.push_back(n);
                    }
                    drop(ring);
                    self.read_events.fetch_add(1, Ordering::AcqRel);
                    self.read_wq.notify_all();
                    crate::task::notify_poll();
                    return Ok(n);
                }
            }
            if nonblock {
                return Err(Error::WouldBlock);
            }
            block_on(&self.write_wq, deadline)?;
        }
    }
}

/// Park the current task on `queue` until a notification, a signal
/// ([`Error::Interrupted`]) or `deadline` (absolute ticks; `None` waits for
/// ever). A deadline already reached is [`Error::WouldBlock`] at once; one
/// that passes while parked returns `Ok` so the caller looks at its ring once
/// more and only then, finding it still empty (or full), gives up here.
fn block_on(queue: &WaitQueue, deadline: Option<u64>) -> Result<(), Error> {
    if deadline.is_some_and(|at| crate::task::ticks() >= at) {
        return Err(Error::WouldBlock);
    }
    match queue.wait(crate::task::current(), deadline) {
        WakeReason::Interrupted => Err(Error::Interrupted),
        WakeReason::Woken | WakeReason::TimedOut => Ok(()),
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        let counter = if self.small {
            &small::LIVE_SMALL
        } else {
            &LIVE_PIPES
        };
        counter.fetch_sub(1, Ordering::AcqRel);
    }
}
