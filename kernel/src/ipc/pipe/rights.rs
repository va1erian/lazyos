//! Descriptors in flight on a socket: `SCM_RIGHTS`.
//!
//! `sendmsg` with a rights message clones the sender's descriptors (taking a
//! reference on each open file description, so the object lives on if the
//! sender closes its copy) into a [`Rights`], which the direction pipe stores
//! beside the bytes of the message: a [`Record`] ties it to the stream
//! positions of those bytes. The receiver's `recvmsg` takes the descriptors
//! out when it consumes the first byte of the message and installs them in its
//! own table (`process::linux::scm`); until then they belong to the queue.
//!
//! The rules follow Linux:
//!
//! * a stream read never mixes bytes of a message that carried descriptors
//!   with others: it stops before such a message, or at its end, so a
//!   descriptor is never delivered with bytes from outside its message; a
//!   seqpacket message may be empty when it carries descriptors;
//! * a read that has nowhere to put descriptors (`read`, `recv`, a `recvmsg`
//!   without a control buffer) still consumes them, closing them;
//! * `MSG_PEEK` hands out duplicates and leaves the queue as it was;
//! * the last reader closing, or the pipe dying, closes whatever is queued;
//! * descriptors in flight count against the sender's `Fds` quota and a
//!   global ceiling, so a process cannot park an unbounded number of open
//!   files in a socket it never reads (`ETOOMANYREFS`).
//!
//! A socket end sent through its own pair, or a socket whose queue already
//! holds descriptors, is refused (`ETOOMANYREFS`): either is the edge that
//! can close a cycle of sockets carrying each other, which nothing would ever
//! read or purge, leaking the in-flight budget for the rest of the boot.
//! Linux reclaims such cycles with a garbage collector; there is none here,
//! and a connection socket is sent with an empty queue (WebKit's pattern),
//! which the rule allows ([`Rights::would_cycle`]).
//!
//! Locking: descriptors are dropped only with the ring unlocked, since
//! closing a socket or pipe end takes other rings' locks and wakes queues.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::quota::{self, Resource};
use crate::task::Fd;

/// Most descriptors one message may carry (Linux's `SCM_MAX_FD`).
pub const SCM_MAX_FD: usize = 253;

/// Most descriptors in flight at once, over all sockets: four full tables.
fn global_cap() -> usize {
    crate::limits::fd_max().saturating_mul(4)
}

/// Descriptors in flight now.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Descriptors in flight on every socket right now (test and soak observable).
pub fn in_flight() -> usize {
    IN_FLIGHT.load(Ordering::Acquire)
}

/// Why descriptors could not be put in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightsError {
    /// More than [`SCM_MAX_FD`] in one message (`EINVAL`).
    TooMany,
    /// The sender's quota or the global ceiling is spent (`ETOOMANYREFS`).
    Quota,
}

/// The descriptors of one message, in flight.
pub struct Rights {
    fds: Vec<Fd>,
    /// The uid whose `Fds` quota holds the charge, and how much was charged.
    uid: u32,
    charged: usize,
}

impl Rights {
    /// Put `fds` in flight, charging `uid`. They are closed if this refuses.
    pub fn new(fds: Vec<Fd>, uid: u32) -> Result<Rights, RightsError> {
        let count = fds.len();
        if count == 0 || count > SCM_MAX_FD {
            return Err(RightsError::TooMany);
        }
        IN_FLIGHT
            .try_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                live.checked_add(count)
                    .filter(|total| *total <= global_cap())
            })
            .map_err(|_| RightsError::Quota)?;
        if quota::charge(uid, Resource::Fds, count as u64).is_err() {
            IN_FLIGHT.fetch_sub(count, Ordering::AcqRel);
            return Err(RightsError::Quota);
        }
        Ok(Rights {
            fds,
            uid,
            charged: count,
        })
    }

    /// How many descriptors this carries.
    pub fn len(&self) -> usize {
        self.fds.len()
    }

    /// Whether queueing these descriptors on `dest` could close a cycle of
    /// sockets carrying each other: one of them is an end of `dest` itself,
    /// or of a pair whose queue already holds descriptors. A cycle needs
    /// every pair in it to carry one, so its last edge always starts from a
    /// pair that already carries, and refusing that edge keeps the graph a
    /// forest without a collector.
    pub fn would_cycle(&self, dest: &SocketPair) -> bool {
        self.fds.iter().any(|fd| match fd {
            Fd::Socket { pair, .. } => core::ptr::eq(&**pair, dest) || pair.carries_rights(),
            _ => false,
        })
    }

    /// The descriptors, handed to their new owner; the in-flight charge ends.
    pub fn into_fds(mut self) -> Vec<Fd> {
        core::mem::take(&mut self.fds)
    }
}

impl Drop for Rights {
    fn drop(&mut self) {
        quota::release(self.uid, Resource::Fds, self.charged as u64);
        IN_FLIGHT.fetch_sub(self.charged, Ordering::AcqRel);
        // `fds` drops after this, closing what was never received.
    }
}

/// A message's descriptors, the stream positions `[start, end)` of its bytes
/// and (seqpacket) its frame number.
pub(super) struct Record {
    start: u64,
    end: u64,
    frame: u64,
    rights: Rights,
}

impl Ring {
    /// How many of `want` bytes a stream read may take. Bytes are never mixed
    /// with a message that carries descriptors (Linux does not glue messages
    /// whose ancillary data differs): a read that starts before one stops where
    /// it begins, and one that starts inside or at it stops where it ends.
    fn rights_cap(&self, want: usize) -> usize {
        if self.mode != Mode::Stream {
            return want;
        }
        let limit = self.total_out + want as u64;
        let Some(record) = self.records.iter().find(|record| record.start < limit) else {
            return want;
        };
        let stop = if record.start > self.total_out {
            record.start
        } else {
            record.end
        };
        want.min(usize::try_from(stop - self.total_out).unwrap_or(want))
    }

    /// The descriptors of seqpacket message `frame`, which a read just took.
    fn take_frame_rights(&mut self, frame: u64) -> Vec<Rights> {
        let mut taken = Vec::new();
        if self
            .records
            .front()
            .is_some_and(|record| record.frame == frame)
        {
            if let Some(record) = self.records.pop_front() {
                taken.push(record.rights);
            }
        }
        taken
    }

    /// Duplicates of the descriptors of the next seqpacket message.
    fn clone_frame_rights(&self, sink: &mut Vec<Fd>) {
        let frame = self.frames_out;
        if let Some(record) = self.records.iter().find(|record| record.frame == frame) {
            if sink.try_reserve(record.rights.len()).is_ok() {
                sink.extend(record.rights.fds.iter().cloned());
            }
        }
    }

    /// Remove the stream messages whose first byte has been consumed.
    fn take_rights(&mut self) -> Vec<Rights> {
        let mut taken = Vec::new();
        while self
            .records
            .front()
            .is_some_and(|record| record.start < self.total_out)
        {
            if let Some(record) = self.records.pop_front() {
                taken.push(record.rights);
            }
        }
        taken
    }

    /// Duplicates of the descriptors of messages starting before stream
    /// position `upto` (what a peek would consume), for `sink`.
    fn clone_rights(&self, upto: u64, sink: &mut Vec<Fd>) {
        for record in self.records.iter().take_while(|record| record.start < upto) {
            if sink.try_reserve(record.rights.len()).is_err() {
                return;
            }
            sink.extend(record.rights.fds.iter().cloned());
        }
    }
}

/// Hand `taken` to `sink`, or close it when there is none.
fn deliver(taken: Vec<Rights>, sink: Option<&mut Vec<Fd>>) {
    match sink {
        Some(sink) => {
            for rights in taken {
                if sink.try_reserve(rights.len()).is_ok() {
                    sink.extend(rights.into_fds());
                }
            }
        }
        None => drop(taken),
    }
}

impl Pipe {
    /// [`Pipe::write_until`] that also queues `rights` beside the bytes it
    /// stores. `rights` is left in place when nothing was stored (an error),
    /// for the caller to close.
    pub fn write_rights(
        &self,
        src: &[u8],
        end: End,
        nonblock: bool,
        deadline: Option<u64>,
        rights: &mut Option<Rights>,
    ) -> Result<usize, Error> {
        if end != End::Write {
            return Err(Error::BadEnd);
        }
        // A message with no bytes carries nothing on a stream; on a seqpacket
        // socket it is a datagram of its own when it has descriptors.
        if src.is_empty() && !(rights.is_some() && self.mode() == Mode::Seqpacket) {
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
                    let was_empty = !ring.has_data();
                    let start = ring.total_in;
                    let n = ring.fill_from(src);
                    let frame = ring.frames_in;
                    if ring.mode == Mode::Seqpacket {
                        ring.frames.push_back(n);
                        ring.frames_in += 1;
                    }
                    if let Some(rights) = rights.take() {
                        let end = ring.total_in;
                        ring.records.push_back(Record {
                            start,
                            end,
                            frame,
                            rights,
                        });
                    }
                    drop(ring);
                    self.read_events.fetch_add(1, Ordering::AcqRel);
                    self.read_wq.notify_all();
                    crate::task::notify_poll_key(self as *const Self as u64);
                    self.bell_after_write(was_empty);
                    return Ok(n);
                }
            }
            if nonblock {
                return Err(Error::WouldBlock);
            }
            block_on(&self.write_wq, deadline)?;
        }
    }

    /// [`Pipe::read_until`] that gives the descriptors of the messages it
    /// consumes to `sink` (closing them when there is none).
    pub fn read_rights(
        &self,
        end: End,
        dst: &mut [u8],
        nonblock: bool,
        deadline: Option<u64>,
        sink: Option<&mut Vec<Fd>>,
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
                let free_before = ring.buf.len() - ring.len;
                if ring.mode == Mode::Seqpacket {
                    if let Some(&message) = ring.frames.front() {
                        let n = message.min(dst.len());
                        let copied = ring.drain_into(&mut dst[..n]);
                        if message > copied {
                            ring.discard(message - copied);
                        }
                        let full = ring.frames.len() >= MAX_FRAMES;
                        ring.frames.pop_front();
                        let frame = ring.frames_out;
                        ring.frames_out += 1;
                        let taken = ring.take_frame_rights(frame);
                        drop(ring);
                        self.after_read(if full { 0 } else { free_before });
                        deliver(taken, sink);
                        return Ok(n);
                    }
                } else if !ring.is_empty() {
                    let want = ring.rights_cap(dst.len());
                    let n = ring.drain_into(&mut dst[..want]);
                    let taken = ring.take_rights();
                    drop(ring);
                    self.after_read(free_before);
                    deliver(taken, sink);
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

    /// [`Pipe::peek_until`] that also copies the descriptors of the messages
    /// it looks at into `sink` (duplicates: the queue keeps its own).
    pub fn peek_rights(
        &self,
        end: End,
        dst: &mut [u8],
        nonblock: bool,
        deadline: Option<u64>,
        sink: Option<&mut Vec<Fd>>,
    ) -> Result<usize, Error> {
        if end != End::Read {
            return Err(Error::BadEnd);
        }
        if dst.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let ring = self.state.lock();
                let seen = if ring.mode == Mode::Seqpacket {
                    ring.frames.front().map(|&message| {
                        let n = message.min(dst.len());
                        (ring.peek_into(&mut dst[..n]), message)
                    })
                } else if !ring.is_empty() {
                    let want = ring.rights_cap(dst.len());
                    let n = ring.peek_into(&mut dst[..want]);
                    Some((n, n))
                } else {
                    None
                };
                if let Some((n, consumed)) = seen {
                    if let Some(sink) = sink {
                        if ring.mode == Mode::Seqpacket {
                            ring.clone_frame_rights(sink);
                        } else {
                            ring.clone_rights(ring.total_out + consumed as u64, sink);
                        }
                    }
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

    /// Close every descriptor still queued (the last reader went away).
    pub(super) fn purge_rights(&self) {
        let records = core::mem::take(&mut self.state.lock().records);
        drop(records);
    }

    /// Messages with descriptors queued.
    pub fn queued_rights(&self) -> usize {
        self.state.lock().records.len()
    }
}

impl SocketPair {
    /// [`SocketPair::write_until`] that queues `rights` beside the bytes.
    pub fn write_rights(
        &self,
        side: Side,
        src: &[u8],
        nonblock: bool,
        deadline: Option<u64>,
        rights: &mut Option<Rights>,
    ) -> Result<usize, Error> {
        if self.is_shutdown(side, 1) {
            return Err(Error::BrokenPipe);
        }
        let (_, write) = self.directions(side);
        write.write_rights(src, End::Write, nonblock, deadline, rights)
    }

    /// A read (or, with `peek`, a peek) of what the peer wrote, giving the
    /// descriptors that came with the bytes to `sink`. `mode` is
    /// `(nonblock, peek)`.
    pub fn recv_rights(
        &self,
        side: Side,
        dst: &mut [u8],
        mode: (bool, bool),
        deadline: Option<u64>,
        sink: Option<&mut Vec<Fd>>,
    ) -> Result<usize, Error> {
        let (nonblock, peek) = mode;
        if self.is_shutdown(side, 0) {
            return Ok(0);
        }
        let (read, _) = self.directions(side);
        if peek {
            read.peek_rights(End::Read, dst, nonblock, deadline, sink)
        } else {
            read.read_rights(End::Read, dst, nonblock, deadline, sink)
        }
    }

    /// Messages with descriptors queued for `side` to read (test observable).
    #[cfg_attr(not(lazyos_tests), allow(dead_code))]
    pub fn queued_rights(&self, side: Side) -> usize {
        self.directions(side).0.queued_rights()
    }

    /// Whether either direction holds descriptors in flight.
    pub fn carries_rights(&self) -> bool {
        let (read, write) = self.directions(Side::A);
        read.queued_rights() + write.queued_rights() > 0
    }
}
