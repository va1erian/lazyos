//! L2CAP wire formats: the basic header and the signalling commands.
//!
//! Everything here is parsed from radio input, so each length is checked
//! against the bytes present and every count is bounded.

use alloc::vec::Vec;

use crate::Error;

pub const HEADER_LEN: usize = 4;

pub const CID_NULL: u16 = 0;
pub const CID_SIGNALLING: u16 = 1;
pub const CID_CONNECTIONLESS: u16 = 2;
pub const CID_ATT: u16 = 4;
pub const CID_LE_SIGNALLING: u16 = 5;
pub const CID_SMP: u16 = 6;
/// First dynamically allocated channel id.
pub const CID_DYNAMIC_MIN: u16 = 0x0040;

/// Most commands one signalling PDU may carry before we call it abuse.
pub const MAX_COMMANDS_PER_PDU: usize = 16;
/// Most configuration options one request or response may carry.
pub const MAX_OPTIONS: usize = 8;

/// Split a basic L2CAP PDU into channel id and payload; the length field must
/// match the bytes present.
pub fn parse_basic(pdu: &[u8]) -> Result<(u16, &[u8]), Error> {
    if pdu.len() < HEADER_LEN {
        return Err(Error::Truncated);
    }
    let len = usize::from(u16::from_le_bytes([pdu[0], pdu[1]]));
    let cid = u16::from_le_bytes([pdu[2], pdu[3]]);
    let payload = &pdu[HEADER_LEN..];
    if payload.len() != len {
        return Err(Error::BadLength);
    }
    Ok((cid, payload))
}

pub fn encode_basic(cid: u16, payload: &[u8]) -> Result<Vec<u8>, Error> {
    let len = u16::try_from(payload.len()).map_err(|_| Error::TooLong)?;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&cid.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

pub mod code {
    pub const COMMAND_REJECT: u8 = 0x01;
    pub const CONNECTION_REQUEST: u8 = 0x02;
    pub const CONNECTION_RESPONSE: u8 = 0x03;
    pub const CONFIGURATION_REQUEST: u8 = 0x04;
    pub const CONFIGURATION_RESPONSE: u8 = 0x05;
    pub const DISCONNECTION_REQUEST: u8 = 0x06;
    pub const DISCONNECTION_RESPONSE: u8 = 0x07;
    pub const ECHO_REQUEST: u8 = 0x08;
    pub const ECHO_RESPONSE: u8 = 0x09;
    pub const INFORMATION_REQUEST: u8 = 0x0A;
    pub const INFORMATION_RESPONSE: u8 = 0x0B;
    pub const CONN_PARAM_UPDATE_REQUEST: u8 = 0x12;
    pub const CONN_PARAM_UPDATE_RESPONSE: u8 = 0x13;
}

/// Connection response results.
pub mod conn_result {
    pub const SUCCESS: u16 = 0;
    pub const PENDING: u16 = 1;
    pub const PSM_NOT_SUPPORTED: u16 = 2;
    pub const SECURITY_BLOCK: u16 = 3;
    pub const NO_RESOURCES: u16 = 4;
    pub const INVALID_SOURCE_CID: u16 = 6;
    pub const SOURCE_CID_ALLOCATED: u16 = 7;
}

/// Configuration response results.
pub mod conf_result {
    pub const SUCCESS: u16 = 0;
    pub const UNACCEPTABLE: u16 = 1;
    pub const REJECTED: u16 = 2;
    pub const UNKNOWN_OPTIONS: u16 = 3;
}

/// Command reject reasons.
pub mod reject {
    pub const NOT_UNDERSTOOD: u16 = 0;
    pub const MTU_EXCEEDED: u16 = 1;
    pub const INVALID_CID: u16 = 2;
}

/// The default and minimum MTU of a BR/EDR channel.
pub const DEFAULT_MTU: u16 = 672;
pub const MIN_MTU: u16 = 48;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfOption {
    Mtu(u16),
    FlushTimeout(u16),
    /// Retransmission and flow control: mode and the eight octets after it.
    Rfc {
        mode: u8,
        rest: [u8; 8],
    },
    Fcs(u8),
    /// Anything else; `kind` keeps the hint bit (0x80).
    Unknown {
        kind: u8,
        data: Vec<u8>,
    },
}

impl ConfOption {
    pub fn is_hint(&self) -> bool {
        matches!(self, ConfOption::Unknown { kind, .. } if kind & 0x80 != 0)
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            ConfOption::Mtu(v) => out.extend_from_slice(&[1, 2, *v as u8, (*v >> 8) as u8]),
            ConfOption::FlushTimeout(v) => {
                out.extend_from_slice(&[2, 2, *v as u8, (*v >> 8) as u8])
            }
            ConfOption::Rfc { mode, rest } => {
                out.extend_from_slice(&[4, 9, *mode]);
                out.extend_from_slice(rest);
            }
            ConfOption::Fcs(v) => out.extend_from_slice(&[5, 1, *v]),
            ConfOption::Unknown { kind, data } => {
                let n = data.len().min(255);
                out.extend_from_slice(&[*kind, n as u8]);
                out.extend_from_slice(&data[..n]);
            }
        }
    }
}

fn parse_options(mut b: &[u8]) -> Result<Vec<ConfOption>, Error> {
    let mut out = Vec::new();
    while !b.is_empty() {
        if out.len() >= MAX_OPTIONS {
            return Err(Error::TooLong);
        }
        let [kind, len, rest @ ..] = b else {
            return Err(Error::Truncated);
        };
        let len = usize::from(*len);
        if rest.len() < len {
            return Err(Error::Truncated);
        }
        let (data, tail) = rest.split_at(len);
        b = tail;
        out.push(match (*kind, data) {
            (1, [a, b]) => ConfOption::Mtu(u16::from_le_bytes([*a, *b])),
            (2, [a, b]) => ConfOption::FlushTimeout(u16::from_le_bytes([*a, *b])),
            (4, [mode, r @ ..]) if r.len() == 8 => {
                let mut rest = [0u8; 8];
                rest.copy_from_slice(r);
                ConfOption::Rfc { mode: *mode, rest }
            }
            (5, [v]) => ConfOption::Fcs(*v),
            // A known type with the wrong length is malformed, not unknown.
            (1 | 2 | 4 | 5, _) => return Err(Error::BadLength),
            (kind, data) => ConfOption::Unknown {
                kind,
                data: data.to_vec(),
            },
        });
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    CommandReject {
        reason: u16,
        data: Vec<u8>,
    },
    ConnectionRequest {
        psm: u16,
        scid: u16,
    },
    ConnectionResponse {
        dcid: u16,
        scid: u16,
        result: u16,
        status: u16,
    },
    ConfigRequest {
        dcid: u16,
        flags: u16,
        options: Vec<ConfOption>,
    },
    ConfigResponse {
        scid: u16,
        flags: u16,
        result: u16,
        options: Vec<ConfOption>,
    },
    DisconnectRequest {
        dcid: u16,
        scid: u16,
    },
    DisconnectResponse {
        dcid: u16,
        scid: u16,
    },
    EchoRequest(Vec<u8>),
    EchoResponse(Vec<u8>),
    InformationRequest {
        info_type: u16,
    },
    InformationResponse {
        info_type: u16,
        result: u16,
        data: Vec<u8>,
    },
    ConnParamUpdateRequest {
        interval_min: u16,
        interval_max: u16,
        latency: u16,
        timeout: u16,
    },
    ConnParamUpdateResponse {
        result: u16,
    },
    Unknown {
        code: u8,
        data: Vec<u8>,
    },
}

/// One signalling command with its identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub ident: u8,
    pub signal: Signal,
}

fn put(d: &mut Vec<u8>, v: u16) {
    d.extend_from_slice(&v.to_le_bytes());
}

fn u16_at(b: &[u8], i: usize) -> Result<u16, Error> {
    match b.get(i..i + 2) {
        Some(s) => Ok(u16::from_le_bytes([s[0], s[1]])),
        None => Err(Error::Truncated),
    }
}

fn exact(b: &[u8], n: usize) -> Result<(), Error> {
    if b.len() == n {
        Ok(())
    } else {
        Err(Error::BadLength)
    }
}

impl Signal {
    fn code(&self) -> u8 {
        use code::*;
        match self {
            Signal::CommandReject { .. } => COMMAND_REJECT,
            Signal::ConnectionRequest { .. } => CONNECTION_REQUEST,
            Signal::ConnectionResponse { .. } => CONNECTION_RESPONSE,
            Signal::ConfigRequest { .. } => CONFIGURATION_REQUEST,
            Signal::ConfigResponse { .. } => CONFIGURATION_RESPONSE,
            Signal::DisconnectRequest { .. } => DISCONNECTION_REQUEST,
            Signal::DisconnectResponse { .. } => DISCONNECTION_RESPONSE,
            Signal::EchoRequest(_) => ECHO_REQUEST,
            Signal::EchoResponse(_) => ECHO_RESPONSE,
            Signal::InformationRequest { .. } => INFORMATION_REQUEST,
            Signal::InformationResponse { .. } => INFORMATION_RESPONSE,
            Signal::ConnParamUpdateRequest { .. } => CONN_PARAM_UPDATE_REQUEST,
            Signal::ConnParamUpdateResponse { .. } => CONN_PARAM_UPDATE_RESPONSE,
            Signal::Unknown { code, .. } => *code,
        }
    }

    fn put_data(&self, d: &mut Vec<u8>) {
        match self {
            Signal::CommandReject { reason, data } => {
                put(d, *reason);
                d.extend_from_slice(data);
            }
            Signal::ConnectionRequest { psm, scid } => {
                put(d, *psm);
                put(d, *scid);
            }
            Signal::ConnectionResponse {
                dcid,
                scid,
                result,
                status,
            } => {
                for v in [dcid, scid, result, status] {
                    put(d, *v);
                }
            }
            Signal::ConfigRequest {
                dcid,
                flags,
                options,
            } => {
                put(d, *dcid);
                put(d, *flags);
                options.iter().for_each(|o| o.encode(d));
            }
            Signal::ConfigResponse {
                scid,
                flags,
                result,
                options,
            } => {
                put(d, *scid);
                put(d, *flags);
                put(d, *result);
                options.iter().for_each(|o| o.encode(d));
            }
            Signal::DisconnectRequest { dcid, scid }
            | Signal::DisconnectResponse { dcid, scid } => {
                put(d, *dcid);
                put(d, *scid);
            }
            Signal::EchoRequest(x) | Signal::EchoResponse(x) => d.extend_from_slice(x),
            Signal::InformationRequest { info_type } => put(d, *info_type),
            Signal::InformationResponse {
                info_type,
                result,
                data,
            } => {
                put(d, *info_type);
                put(d, *result);
                d.extend_from_slice(data);
            }
            Signal::ConnParamUpdateRequest {
                interval_min,
                interval_max,
                latency,
                timeout,
            } => {
                for v in [interval_min, interval_max, latency, timeout] {
                    put(d, *v);
                }
            }
            Signal::ConnParamUpdateResponse { result } => put(d, *result),
            Signal::Unknown { data, .. } => d.extend_from_slice(data),
        }
    }

    fn parse(code: u8, b: &[u8]) -> Result<Signal, Error> {
        use self::code as c;
        Ok(match code {
            c::COMMAND_REJECT => Signal::CommandReject {
                reason: u16_at(b, 0)?,
                data: b[2..].to_vec(),
            },
            c::CONNECTION_REQUEST => {
                exact(b, 4)?;
                Signal::ConnectionRequest {
                    psm: u16_at(b, 0)?,
                    scid: u16_at(b, 2)?,
                }
            }
            c::CONNECTION_RESPONSE => {
                exact(b, 8)?;
                Signal::ConnectionResponse {
                    dcid: u16_at(b, 0)?,
                    scid: u16_at(b, 2)?,
                    result: u16_at(b, 4)?,
                    status: u16_at(b, 6)?,
                }
            }
            c::CONFIGURATION_REQUEST => Signal::ConfigRequest {
                dcid: u16_at(b, 0)?,
                flags: u16_at(b, 2)?,
                options: parse_options(&b[4..])?,
            },
            c::CONFIGURATION_RESPONSE => Signal::ConfigResponse {
                scid: u16_at(b, 0)?,
                flags: u16_at(b, 2)?,
                result: u16_at(b, 4)?,
                options: parse_options(&b[6..])?,
            },
            c::DISCONNECTION_REQUEST | c::DISCONNECTION_RESPONSE => {
                exact(b, 4)?;
                let (dcid, scid) = (u16_at(b, 0)?, u16_at(b, 2)?);
                if code == c::DISCONNECTION_REQUEST {
                    Signal::DisconnectRequest { dcid, scid }
                } else {
                    Signal::DisconnectResponse { dcid, scid }
                }
            }
            c::ECHO_REQUEST => Signal::EchoRequest(b.to_vec()),
            c::ECHO_RESPONSE => Signal::EchoResponse(b.to_vec()),
            c::INFORMATION_REQUEST => {
                exact(b, 2)?;
                Signal::InformationRequest {
                    info_type: u16_at(b, 0)?,
                }
            }
            c::INFORMATION_RESPONSE => Signal::InformationResponse {
                info_type: u16_at(b, 0)?,
                result: u16_at(b, 2)?,
                data: b[4..].to_vec(),
            },
            c::CONN_PARAM_UPDATE_REQUEST => {
                exact(b, 8)?;
                Signal::ConnParamUpdateRequest {
                    interval_min: u16_at(b, 0)?,
                    interval_max: u16_at(b, 2)?,
                    latency: u16_at(b, 4)?,
                    timeout: u16_at(b, 6)?,
                }
            }
            c::CONN_PARAM_UPDATE_RESPONSE => {
                exact(b, 2)?;
                Signal::ConnParamUpdateResponse {
                    result: u16_at(b, 0)?,
                }
            }
            _ => Signal::Unknown {
                code,
                data: b.to_vec(),
            },
        })
    }
}

impl Command {
    /// One command: `code, ident, length:u16, data`.
    pub fn encode(&self) -> Vec<u8> {
        let mut data = Vec::new();
        self.signal.put_data(&mut data);
        data.truncate(usize::from(u16::MAX));
        let mut out = Vec::with_capacity(4 + data.len());
        out.push(self.signal.code());
        out.push(self.ident);
        out.extend_from_slice(&(data.len() as u16).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }
}

/// All commands in a signalling PDU payload. An ident of zero is invalid.
pub fn parse_commands(mut b: &[u8]) -> Result<Vec<Command>, Error> {
    let mut out = Vec::new();
    while !b.is_empty() {
        if out.len() >= MAX_COMMANDS_PER_PDU {
            return Err(Error::TooLong);
        }
        if b.len() < 4 {
            return Err(Error::Truncated);
        }
        let len = usize::from(u16::from_le_bytes([b[2], b[3]]));
        let body = b.get(4..4 + len).ok_or(Error::Truncated)?;
        if b[1] == 0 {
            return Err(Error::Invalid);
        }
        out.push(Command {
            ident: b[1],
            signal: Signal::parse(b[0], body)?,
        });
        b = &b[4 + len..];
    }
    Ok(out)
}
