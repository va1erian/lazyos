//! ACL fragmentation and reassembly with bounded buffers.
//!
//! An L2CAP PDU travels in one or more ACL packets (first fragment, then
//! continuations). The reassembler never holds more than `max_pdu` octets and
//! never trusts the length in the first fragment: a PDU announcing more than
//! the bound is refused at once and its continuations are swallowed by
//! counting, so a hostile length cannot make us allocate.

use alloc::vec::Vec;
use bthci::Boundary;

use crate::pdu::HEADER_LEN;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragError {
    /// The announced PDU is larger than the buffer bound.
    Oversize,
    /// A continuation with no first fragment before it.
    UnexpectedContinuation,
    /// More data arrived than the header announced.
    Overrun,
    /// The reserved boundary value.
    BadBoundary,
}

#[derive(Debug)]
enum State {
    Idle,
    Collecting {
        buf: Vec<u8>,
        need: Option<usize>,
    },
    /// Throwing away the rest of a refused PDU.
    Skipping {
        remaining: usize,
    },
}

#[derive(Debug)]
pub struct Reassembler {
    max_pdu: usize,
    state: State,
    /// Incomplete PDUs dropped because a new first fragment arrived.
    pub dropped_incomplete: u32,
}

impl Reassembler {
    /// `max_pdu` bounds the whole PDU including the 4-octet header.
    pub fn new(max_pdu: usize) -> Reassembler {
        Reassembler {
            max_pdu: max_pdu.max(HEADER_LEN),
            state: State::Idle,
            dropped_incomplete: 0,
        }
    }

    /// Feed one ACL fragment. `Ok(Some(pdu))` is a complete L2CAP PDU
    /// (header included); `Ok(None)` wants more.
    pub fn push(&mut self, boundary: Boundary, data: &[u8]) -> Result<Option<Vec<u8>>, FragError> {
        match boundary {
            Boundary::Reserved => Err(FragError::BadBoundary),
            Boundary::Continuing => self.continuation(data),
            Boundary::FirstFlushable | Boundary::FirstNonFlushable => {
                if matches!(self.state, State::Collecting { .. }) {
                    self.dropped_incomplete = self.dropped_incomplete.saturating_add(1);
                }
                self.state = State::Collecting {
                    buf: Vec::new(),
                    need: None,
                };
                self.append(data)
            }
        }
    }

    fn continuation(&mut self, data: &[u8]) -> Result<Option<Vec<u8>>, FragError> {
        // An empty fragment carries nothing to misplace.
        if data.is_empty() {
            return Ok(None);
        }
        match &mut self.state {
            State::Idle => Err(FragError::UnexpectedContinuation),
            State::Collecting { .. } => self.append(data),
            State::Skipping { remaining } => {
                if data.len() > *remaining {
                    self.state = State::Idle;
                    return Err(FragError::Overrun);
                }
                *remaining -= data.len();
                if *remaining == 0 {
                    self.state = State::Idle;
                }
                Ok(None)
            }
        }
    }

    fn append(&mut self, mut data: &[u8]) -> Result<Option<Vec<u8>>, FragError> {
        let max_pdu = self.max_pdu;
        let State::Collecting { buf, need } = &mut self.state else {
            return Ok(None);
        };
        // The header may itself be split; commit to a size only once known.
        if need.is_none() {
            let n = data.len().min(HEADER_LEN - buf.len());
            buf.extend_from_slice(&data[..n]);
            data = &data[n..];
            if buf.len() < HEADER_LEN {
                return Ok(None);
            }
            let body = usize::from(u16::from_le_bytes([buf[0], buf[1]]));
            if HEADER_LEN + body > max_pdu {
                return Err(self.refuse(body, data.len()));
            }
            *need = Some(HEADER_LEN + body);
        }
        let Some(total) = *need else {
            return Ok(None);
        };
        if buf.len() + data.len() > total {
            self.state = State::Idle;
            return Err(FragError::Overrun);
        }
        buf.extend_from_slice(data);
        if buf.len() < total {
            return Ok(None);
        }
        match core::mem::replace(&mut self.state, State::Idle) {
            State::Collecting { buf, .. } => Ok(Some(buf)),
            _ => Ok(None),
        }
    }

    /// Refuse an announced PDU whose body is `body` octets of which `seen`
    /// are already here; swallow the rest by count.
    fn refuse(&mut self, body: usize, seen: usize) -> FragError {
        if seen > body {
            self.state = State::Idle;
            return FragError::Overrun;
        }
        self.state = match body - seen {
            0 => State::Idle,
            remaining => State::Skipping { remaining },
        };
        FragError::Oversize
    }
}

/// The ACL packets for one PDU: `(boundary, payload)` pieces of at most
/// `max_len` octets (the controller's ACL data length); the first is marked as
/// a start, the rest continue.
pub fn fragments(pdu: &[u8], max_len: usize) -> impl Iterator<Item = (Boundary, &[u8])> {
    let max_len = max_len.max(1);
    pdu.chunks(max_len).enumerate().map(|(i, chunk)| {
        let b = if i == 0 {
            Boundary::FirstNonFlushable
        } else {
            Boundary::Continuing
        };
        (b, chunk)
    })
}
