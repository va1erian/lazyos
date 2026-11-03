//! The ATT client: one request outstanding, answered, an Error Response, or
//! the 30 second transaction timeout (Core Specification, Vol 3 Part F 3.3).
//!
//! No clock and no I/O: the caller passes the time, carries the PDUs over
//! L2CAP fixed channel 4 and calls [`Client::tick`] when [`Client::deadline`]
//! passes. After a timeout or a protocol violation the client is dead: the
//! specification forbids further ATT traffic on that bearer, and the caller
//! drops the link.

use alloc::vec::Vec;

use crate::att::{
    self, error_pdu, op, parse_error, parse_incoming, Incoming, Request, Response, DEFAULT_MTU,
    MAX_MTU,
};
use crate::Error;

/// ATT transaction timeout.
pub const ATT_TIMEOUT_MS: u64 = 30_000;

/// What a received PDU meant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Not for us (a stray response, an ignorable command).
    Ignored,
    Response {
        request: Request,
        response: Response,
    },
    /// The server refused the request.
    Error {
        request: Request,
        handle: u16,
        code: u8,
    },
    Notification {
        handle: u16,
        value: Vec<u8>,
    },
    /// The conversation is over; drop the link.
    Failed(Error),
}

/// An [`Event`] and the PDU to send back, if the protocol asks for one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    pub event: Event,
    pub reply: Option<Vec<u8>>,
}

pub struct Client {
    mtu: u16,
    rx_mtu: u16,
    pending: Option<(Request, u64)>,
    dead: bool,
}

impl Client {
    /// A client whose own receive MTU (offered in Exchange MTU) is `rx_mtu`,
    /// clamped to the protocol's range.
    pub fn new(rx_mtu: u16) -> Client {
        Client {
            mtu: DEFAULT_MTU,
            rx_mtu: rx_mtu.clamp(DEFAULT_MTU, MAX_MTU),
            pending: None,
            dead: false,
        }
    }

    /// The MTU in force (23 until an exchange succeeds).
    pub fn mtu(&self) -> u16 {
        self.mtu
    }

    /// The MTU this client offers.
    pub fn rx_mtu(&self) -> u16 {
        self.rx_mtu
    }

    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// When [`Client::tick`] must be called: the transaction timeout.
    pub fn deadline(&self) -> Option<u64> {
        self.pending.as_ref().map(|(_, due)| *due)
    }

    /// Start `request`; the returned PDU goes out on the ATT channel.
    pub fn send(&mut self, now: u64, request: Request) -> Result<Vec<u8>, Error> {
        if self.dead {
            return Err(Error::Dead);
        }
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        if let Request::Write { value, .. } = &request {
            // Opcode and handle take three octets of the MTU.
            if value.len() + 3 > usize::from(self.mtu) {
                return Err(Error::TooLong);
            }
        }
        let pdu = request.encode();
        self.pending = Some((request, now.saturating_add(ATT_TIMEOUT_MS)));
        Ok(pdu)
    }

    /// A Write Command: no response, so it does not occupy the client.
    pub fn write_command(&self, handle: u16, value: &[u8]) -> Result<Vec<u8>, Error> {
        if self.dead {
            return Err(Error::Dead);
        }
        if value.len() + 3 > usize::from(self.mtu) {
            return Err(Error::TooLong);
        }
        let mut pdu = Vec::with_capacity(3 + value.len());
        pdu.push(op::WRITE_CMD);
        pdu.extend_from_slice(&handle.to_le_bytes());
        pdu.extend_from_slice(value);
        Ok(pdu)
    }

    /// The time has reached [`Client::deadline`]: a request unanswered for 30
    /// seconds kills the client.
    pub fn tick(&mut self, now: u64) -> Option<Error> {
        let (_, due) = self.pending.as_ref()?;
        if now < *due {
            return None;
        }
        self.pending = None;
        self.dead = true;
        Some(Error::Timeout)
    }

    /// One PDU from the server.
    pub fn on_pdu(&mut self, pdu: &[u8]) -> Received {
        if self.dead {
            return Received {
                event: Event::Ignored,
                reply: None,
            };
        }
        let first = pdu.first().copied().unwrap_or(0);
        let answer = match &self.pending {
            Some(_) if first == op::ERROR => Some(true),
            Some((request, _)) if first == request.opcode() + 1 => Some(false),
            _ => None,
        };
        match answer {
            Some(is_error) => self.answer(pdu, is_error),
            None => self.unsolicited(pdu),
        }
    }

    fn fail(&mut self, error: Error) -> Received {
        self.pending = None;
        self.dead = true;
        Received {
            event: Event::Failed(error),
            reply: None,
        }
    }

    fn answer(&mut self, pdu: &[u8], is_error: bool) -> Received {
        let Some((request, _)) = self.pending.take() else {
            return self.unsolicited(pdu);
        };
        let event = if is_error {
            match parse_error(pdu) {
                Ok(e) if e.request_opcode == request.opcode() => Event::Error {
                    request,
                    handle: e.handle,
                    code: e.code,
                },
                // An error for some other request: the server is confused.
                _ => return self.fail(Error::Malformed),
            }
        } else {
            match Response::parse(&request, pdu, self.mtu) {
                Ok(response) => {
                    if let Response::Mtu(server) = &response {
                        self.mtu = (*server).max(DEFAULT_MTU).min(self.rx_mtu);
                    }
                    Event::Response { request, response }
                }
                Err(e) => return self.fail(e),
            }
        };
        Received { event, reply: None }
    }

    fn unsolicited(&mut self, pdu: &[u8]) -> Received {
        let ignored = Received {
            event: Event::Ignored,
            reply: None,
        };
        match parse_incoming(pdu) {
            Ok(Incoming::Notification { handle, value }) => {
                if value.len() + 3 > usize::from(self.mtu) {
                    return self.fail(Error::TooLong);
                }
                Received {
                    event: Event::Notification { handle, value },
                    reply: None,
                }
            }
            // We subscribe to notifications only; an indication is still
            // confirmed, or the server's next one stalls forever.
            Ok(Incoming::Indication { .. }) => Received {
                event: Event::Ignored,
                reply: Some(alloc::vec![op::CONFIRM]),
            },
            Ok(Incoming::Request(opcode)) => Received {
                event: Event::Ignored,
                reply: Some(error_pdu(opcode, 0, att::err::REQUEST_NOT_SUPPORTED)),
            },
            Ok(Incoming::Ignored) => ignored,
            Err(_) => ignored,
        }
    }
}
