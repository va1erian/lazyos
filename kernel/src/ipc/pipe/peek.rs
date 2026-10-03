//! `MSG_PEEK`: read a pipe's (or a socket direction's) queued bytes without
//! consuming them. Blocking, end-of-file and seqpacket framing behave exactly
//! as in [`Pipe::read`]; only the head of the ring stays where it was.

use super::*;

impl Ring {
    /// Copy the oldest `min(len, dst.len())` bytes out without advancing.
    fn peek_into(&self, dst: &mut [u8]) -> usize {
        let n = self.len.min(dst.len());
        let first = (self.buf.len() - self.head).min(n);
        dst[..first].copy_from_slice(&self.buf[self.head..self.head + first]);
        if n > first {
            dst[first..n].copy_from_slice(&self.buf[..n - first]);
        }
        n
    }
}

impl Pipe {
    /// Like [`Pipe::read`], but the bytes stay queued. A seqpacket peek
    /// returns (a prefix of) the oldest message.
    pub fn peek(&self, end: End, dst: &mut [u8], nonblock: bool) -> Result<usize, Error> {
        if end != End::Read {
            return Err(Error::BadEnd);
        }
        if dst.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let ring = self.state.lock();
                if ring.mode == Mode::Seqpacket {
                    if let Some(&message) = ring.frames.front() {
                        let n = message.min(dst.len());
                        return Ok(ring.peek_into(&mut dst[..n]));
                    }
                } else if !ring.is_empty() {
                    return Ok(ring.peek_into(dst));
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

    /// Bytes queued for reading (the `FIONREAD` answer); for a seqpacket
    /// pipe, the size of the next message.
    pub fn queued(&self) -> usize {
        let ring = self.state.lock();
        if ring.mode == Mode::Seqpacket {
            ring.frames.front().copied().unwrap_or(0)
        } else {
            ring.len
        }
    }
}

impl SocketPair {
    /// [`Pipe::peek`] on the direction `side` reads from.
    pub fn peek(&self, side: Side, dst: &mut [u8], nonblock: bool) -> Result<usize, Error> {
        if self.is_shutdown(side, 0) {
            return Ok(0);
        }
        let (read, _) = self.directions(side);
        read.peek(End::Read, dst, nonblock)
    }

    /// Bytes queued for `side` to read.
    pub fn queued(&self, side: Side) -> usize {
        self.directions(side).0.queued()
    }
}
