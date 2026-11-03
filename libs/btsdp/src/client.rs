//! The search conversation: one `ServiceSearchAttributeRequest` for the HID
//! service class, then continuations until the server says it is done.

use alloc::vec::Vec;

use crate::record::{parse_attribute_lists, HidRecord, UUID_HID};
use crate::{Error, MAX_ATTRIBUTE_BYTES, MAX_CONTINUATION, MAX_ROUNDS};

const PDU_ERROR_RESPONSE: u8 = 0x01;
const PDU_SEARCH_ATTRIBUTE_REQUEST: u8 = 0x06;
const PDU_SEARCH_ATTRIBUTE_RESPONSE: u8 = 0x07;
/// PDU header: id, transaction id, parameter length.
const HEADER: usize = 5;
/// The most attribute bytes one response may carry: what fits an L2CAP
/// channel at its default MTU (672) with the PDU framing around it.
const MAX_BYTES_PER_RESPONSE: u16 = 0x0200;

/// What the caller does next.
#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    /// Send this PDU on the SDP channel and feed the answer to
    /// [`Client::on_pdu`].
    Request(Vec<u8>),
    /// The search is over: the device's HID record.
    Done(HidRecord),
}

/// A single search for the HID record of one device.
#[derive(Debug, Default)]
pub struct Client {
    tid: u16,
    attributes: Vec<u8>,
    /// The continuation state of the request in flight (empty: the first).
    continuation: Vec<u8>,
    rounds: u8,
    awaiting: bool,
}

impl Client {
    pub fn new() -> Client {
        Client::default()
    }

    /// The first request.
    pub fn start(&mut self) -> Vec<u8> {
        *self = Client {
            tid: self.tid,
            ..Client::default()
        };
        self.request()
    }

    /// Whether a response is expected (a request is in flight).
    pub fn awaiting(&self) -> bool {
        self.awaiting
    }

    fn request(&mut self) -> Vec<u8> {
        self.tid = self.tid.wrapping_add(1);
        self.awaiting = true;
        // Service search pattern: the HID service class. Attribute ids: the
        // class list, the primary-language name and the HID range 0x0200 to
        // 0x020E, which holds every attribute `HidRecord` reads.
        let mut params = Vec::new();
        params.extend_from_slice(&[0x35, 0x03, 0x19]);
        params.extend_from_slice(&UUID_HID.to_be_bytes());
        params.extend_from_slice(&MAX_BYTES_PER_RESPONSE.to_be_bytes());
        params.extend_from_slice(&[
            0x35, 0x0B, 0x09, 0x00, 0x01, 0x09, 0x01, 0x00, 0x0A, 0x02, 0x00, 0x02, 0x0E,
        ]);
        params.push(self.continuation.len() as u8);
        params.extend_from_slice(&self.continuation);
        let mut pdu = Vec::with_capacity(HEADER + params.len());
        pdu.push(PDU_SEARCH_ATTRIBUTE_REQUEST);
        pdu.extend_from_slice(&self.tid.to_be_bytes());
        pdu.extend_from_slice(&(params.len() as u16).to_be_bytes());
        pdu.extend_from_slice(&params);
        pdu
    }

    /// Feed one response PDU.
    pub fn on_pdu(&mut self, pdu: &[u8]) -> Result<Progress, Error> {
        let result = self.step(pdu);
        if !matches!(result, Ok(Progress::Request(_))) {
            self.awaiting = false;
        }
        result
    }

    fn step(&mut self, pdu: &[u8]) -> Result<Progress, Error> {
        let [id, t0, t1, l0, l1, params @ ..] = pdu else {
            return Err(Error::Truncated);
        };
        if !self.awaiting || u16::from_be_bytes([*t0, *t1]) != self.tid {
            return Err(Error::BadTransaction);
        }
        if usize::from(u16::from_be_bytes([*l0, *l1])) != params.len() {
            return Err(Error::BadPdu);
        }
        match *id {
            PDU_ERROR_RESPONSE => match params {
                [a, b] => Err(Error::Remote(u16::from_be_bytes([*a, *b]))),
                _ => Err(Error::BadPdu),
            },
            PDU_SEARCH_ATTRIBUTE_RESPONSE => self.on_response(params),
            _ => Err(Error::BadPdu),
        }
    }

    fn on_response(&mut self, params: &[u8]) -> Result<Progress, Error> {
        let [c0, c1, rest @ ..] = params else {
            return Err(Error::Truncated);
        };
        let count = usize::from(u16::from_be_bytes([*c0, *c1]));
        if rest.len() <= count {
            return Err(Error::BadPdu);
        }
        let (data, tail) = rest.split_at(count);
        let (&state_len, state) = tail.split_first().ok_or(Error::Truncated)?;
        if usize::from(state_len) != state.len() || state.len() > MAX_CONTINUATION {
            return Err(Error::BadPdu);
        }
        if self.attributes.len() + data.len() > MAX_ATTRIBUTE_BYTES {
            return Err(Error::TooLarge);
        }
        self.attributes.extend_from_slice(data);
        if state.is_empty() {
            return parse_attribute_lists(&self.attributes).map(Progress::Done);
        }
        // More to come: it must have made progress, and there is a limit to
        // how long a server may talk.
        self.rounds += 1;
        if self.rounds >= MAX_ROUNDS {
            return Err(Error::TooManyRounds);
        }
        if data.is_empty() {
            return Err(Error::NoProgress);
        }
        self.continuation = state.to_vec();
        Ok(Progress::Request(self.request()))
    }
}
