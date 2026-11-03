//! HCI event packets (`code:u8, length:u8, parameters`).
//!
//! The controller (or, on the radio, a stranger relaying through it) writes
//! these, so [`Event::parse`] checks every length. Known events with a
//! malformed body are an error; unknown event codes are kept verbatim as
//! [`Event::Unknown`] so a newer controller's extra events do not break the
//! host. Trailing bytes after a fixed layout are ignored: newer
//! specifications extend events at the end.

use alloc::vec::Vec;

use crate::addr::BdAddr;
use crate::opcode as op;
use crate::wire::{put_u16, put_u24, put_u32, put_u64, Reader};
use crate::Error;

/// An LE advertising report holds at most this many entries per event
/// (Core Specification: 1 to 25).
pub const MAX_ADV_REPORTS: usize = 25;
/// Legacy advertising data is at most 31 octets.
pub const MAX_ADV_DATA: usize = 31;
/// Entries one Number_Of_Completed_Packets event can carry.
pub const MAX_COMPLETED_ENTRIES: usize = 255;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvReport {
    pub event_type: u8,
    pub address_type: u8,
    pub addr: BdAddr,
    pub data: Vec<u8>,
    pub rssi: i8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    CommandComplete {
        num_commands: u8,
        opcode: u16,
        return_params: Vec<u8>,
    },
    CommandStatus {
        status: u8,
        num_commands: u8,
        opcode: u16,
    },
    ConnectionRequest {
        addr: BdAddr,
        class_of_device: u32,
        link_type: u8,
    },
    ConnectionComplete {
        status: u8,
        handle: u16,
        addr: BdAddr,
        link_type: u8,
        encryption_enabled: bool,
    },
    DisconnectionComplete {
        status: u8,
        handle: u16,
        reason: u8,
    },
    AuthenticationComplete {
        status: u8,
        handle: u16,
    },
    EncryptionChange {
        status: u8,
        handle: u16,
        enabled: bool,
    },
    /// `(handle, completed packets)` pairs.
    NumberOfCompletedPackets(Vec<(u16, u16)>),
    LinkKeyRequest {
        addr: BdAddr,
    },
    LinkKeyNotification {
        addr: BdAddr,
        key: [u8; 16],
        key_type: u8,
    },
    IoCapabilityRequest {
        addr: BdAddr,
    },
    IoCapabilityResponse {
        addr: BdAddr,
        io_capability: u8,
        oob_present: u8,
        authentication_requirements: u8,
    },
    UserConfirmationRequest {
        addr: BdAddr,
        numeric_value: u32,
    },
    UserPasskeyRequest {
        addr: BdAddr,
    },
    UserPasskeyNotification {
        addr: BdAddr,
        passkey: u32,
    },
    SimplePairingComplete {
        status: u8,
        addr: BdAddr,
    },
    LeConnectionComplete(LeConnection),
    /// Same fields as [`Event::LeConnectionComplete`] plus the resolvable
    /// private addresses used on the link (all zero when none).
    LeEnhancedConnectionComplete {
        conn: LeConnection,
        local_rpa: BdAddr,
        peer_rpa: BdAddr,
    },
    LeAdvertisingReport(Vec<AdvReport>),
    LeLongTermKeyRequest {
        handle: u16,
        random: u64,
        ediv: u16,
    },
    UnknownLe {
        subevent: u8,
        params: Vec<u8>,
    },
    Unknown {
        code: u8,
        params: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeConnection {
    pub status: u8,
    pub handle: u16,
    /// 0: we are the central.
    pub role: u8,
    pub peer_address_type: u8,
    pub peer: BdAddr,
    pub interval: u16,
    pub latency: u16,
    pub supervision_timeout: u16,
    pub clock_accuracy: u8,
}

impl LeConnection {
    fn parse(r: &mut Reader<'_>) -> Result<LeConnection, Error> {
        Ok(LeConnection {
            status: r.u8()?,
            handle: r.u16()?,
            role: r.u8()?,
            peer_address_type: r.u8()?,
            peer: r.addr()?,
            interval: 0,
            latency: 0,
            supervision_timeout: 0,
            clock_accuracy: 0,
        })
    }

    /// The timing tail, which follows the RPAs in the enhanced layout.
    fn parse_timing(&mut self, r: &mut Reader<'_>) -> Result<(), Error> {
        self.interval = r.u16()?;
        self.latency = r.u16()?;
        self.supervision_timeout = r.u16()?;
        self.clock_accuracy = r.u8()?;
        Ok(())
    }

    fn put_head(&self, p: &mut Vec<u8>) {
        p.push(self.status);
        put_u16(p, self.handle);
        p.extend_from_slice(&[self.role, self.peer_address_type]);
        p.extend_from_slice(&self.peer.0);
    }

    fn put_timing(&self, p: &mut Vec<u8>) {
        put_u16(p, self.interval);
        put_u16(p, self.latency);
        put_u16(p, self.supervision_timeout);
        p.push(self.clock_accuracy);
    }
}

impl Event {
    /// The event code on the wire (`0x3E` for every LE subevent).
    pub fn code(&self) -> u8 {
        use Event::*;
        match self {
            CommandComplete { .. } => op::EV_COMMAND_COMPLETE,
            CommandStatus { .. } => op::EV_COMMAND_STATUS,
            ConnectionRequest { .. } => op::EV_CONNECTION_REQUEST,
            ConnectionComplete { .. } => op::EV_CONNECTION_COMPLETE,
            DisconnectionComplete { .. } => op::EV_DISCONNECTION_COMPLETE,
            AuthenticationComplete { .. } => op::EV_AUTHENTICATION_COMPLETE,
            EncryptionChange { .. } => op::EV_ENCRYPTION_CHANGE,
            NumberOfCompletedPackets(_) => op::EV_NUMBER_OF_COMPLETED_PACKETS,
            LinkKeyRequest { .. } => op::EV_LINK_KEY_REQUEST,
            LinkKeyNotification { .. } => op::EV_LINK_KEY_NOTIFICATION,
            IoCapabilityRequest { .. } => op::EV_IO_CAPABILITY_REQUEST,
            IoCapabilityResponse { .. } => op::EV_IO_CAPABILITY_RESPONSE,
            UserConfirmationRequest { .. } => op::EV_USER_CONFIRMATION_REQUEST,
            UserPasskeyRequest { .. } => op::EV_USER_PASSKEY_REQUEST,
            UserPasskeyNotification { .. } => op::EV_USER_PASSKEY_NOTIFICATION,
            SimplePairingComplete { .. } => op::EV_SIMPLE_PAIRING_COMPLETE,
            LeConnectionComplete(_)
            | LeEnhancedConnectionComplete { .. }
            | LeAdvertisingReport(_)
            | LeLongTermKeyRequest { .. }
            | UnknownLe { .. } => op::EV_LE_META,
            Unknown { code, .. } => *code,
        }
    }

    /// The complete event packet. Oversized variable parts are cut to the
    /// wire's limits.
    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::new();
        self.put_params(&mut p);
        p.truncate(255);
        let mut out = Vec::with_capacity(2 + p.len());
        out.push(self.code());
        out.push(p.len() as u8);
        out.extend_from_slice(&p);
        out
    }

    fn put_params(&self, p: &mut Vec<u8>) {
        use Event::*;
        match self {
            CommandComplete {
                num_commands,
                opcode,
                return_params,
            } => {
                p.push(*num_commands);
                put_u16(p, *opcode);
                p.extend_from_slice(return_params);
            }
            CommandStatus {
                status,
                num_commands,
                opcode,
            } => {
                p.extend_from_slice(&[*status, *num_commands]);
                put_u16(p, *opcode);
            }
            ConnectionRequest {
                addr,
                class_of_device,
                link_type,
            } => {
                p.extend_from_slice(&addr.0);
                put_u24(p, *class_of_device);
                p.push(*link_type);
            }
            ConnectionComplete {
                status,
                handle,
                addr,
                link_type,
                encryption_enabled,
            } => {
                p.push(*status);
                put_u16(p, *handle);
                p.extend_from_slice(&addr.0);
                p.extend_from_slice(&[*link_type, u8::from(*encryption_enabled)]);
            }
            DisconnectionComplete {
                status,
                handle,
                reason,
            } => {
                p.push(*status);
                put_u16(p, *handle);
                p.push(*reason);
            }
            AuthenticationComplete { status, handle } => {
                p.push(*status);
                put_u16(p, *handle);
            }
            EncryptionChange {
                status,
                handle,
                enabled,
            } => {
                p.push(*status);
                put_u16(p, *handle);
                p.push(u8::from(*enabled));
            }
            NumberOfCompletedPackets(entries) => {
                let n = entries.len().min(MAX_COMPLETED_ENTRIES / 2);
                p.push(n as u8);
                for (h, _) in &entries[..n] {
                    put_u16(p, *h);
                }
                for (_, c) in &entries[..n] {
                    put_u16(p, *c);
                }
            }
            LinkKeyRequest { addr }
            | IoCapabilityRequest { addr }
            | UserPasskeyRequest { addr } => p.extend_from_slice(&addr.0),
            LinkKeyNotification {
                addr,
                key,
                key_type,
            } => {
                p.extend_from_slice(&addr.0);
                p.extend_from_slice(key);
                p.push(*key_type);
            }
            IoCapabilityResponse {
                addr,
                io_capability,
                oob_present,
                authentication_requirements,
            } => {
                p.extend_from_slice(&addr.0);
                p.extend_from_slice(&[*io_capability, *oob_present, *authentication_requirements]);
            }
            UserConfirmationRequest {
                addr,
                numeric_value: v,
            }
            | UserPasskeyNotification { addr, passkey: v } => {
                p.extend_from_slice(&addr.0);
                put_u32(p, *v);
            }
            SimplePairingComplete { status, addr } => {
                p.push(*status);
                p.extend_from_slice(&addr.0);
            }
            LeConnectionComplete(c) => {
                p.push(op::LE_CONNECTION_COMPLETE);
                c.put_head(p);
                c.put_timing(p);
            }
            LeEnhancedConnectionComplete {
                conn,
                local_rpa,
                peer_rpa,
            } => {
                p.push(op::LE_ENHANCED_CONNECTION_COMPLETE);
                conn.put_head(p);
                p.extend_from_slice(&local_rpa.0);
                p.extend_from_slice(&peer_rpa.0);
                conn.put_timing(p);
            }
            LeAdvertisingReport(reports) => {
                p.push(op::LE_ADVERTISING_REPORT);
                let n = reports.len().min(MAX_ADV_REPORTS);
                p.push(n as u8);
                for r in &reports[..n] {
                    p.extend_from_slice(&[r.event_type, r.address_type]);
                    p.extend_from_slice(&r.addr.0);
                    let d = r.data.len().min(MAX_ADV_DATA);
                    p.push(d as u8);
                    p.extend_from_slice(&r.data[..d]);
                    p.push(r.rssi as u8);
                }
            }
            LeLongTermKeyRequest {
                handle,
                random,
                ediv,
            } => {
                p.push(op::LE_LONG_TERM_KEY_REQUEST);
                put_u16(p, *handle);
                put_u64(p, *random);
                put_u16(p, *ediv);
            }
            UnknownLe { subevent, params } => {
                p.push(*subevent);
                p.extend_from_slice(params);
            }
            Unknown { params, .. } => p.extend_from_slice(params),
        }
    }

    /// Parse a complete event packet; the length octet must match.
    pub fn parse(packet: &[u8]) -> Result<Event, Error> {
        let mut r = Reader::new(packet);
        let code = r.u8()?;
        let len = usize::from(r.u8()?);
        if r.remaining() != len {
            return Err(Error::BadLength);
        }
        Event::parse_params(code, r.rest())
    }

    fn parse_params(code: u8, params: &[u8]) -> Result<Event, Error> {
        use Event::*;
        let mut r = Reader::new(params);
        Ok(match code {
            op::EV_COMMAND_COMPLETE => CommandComplete {
                num_commands: r.u8()?,
                opcode: r.u16()?,
                return_params: r.rest().to_vec(),
            },
            op::EV_COMMAND_STATUS => CommandStatus {
                status: r.u8()?,
                num_commands: r.u8()?,
                opcode: r.u16()?,
            },
            op::EV_CONNECTION_REQUEST => ConnectionRequest {
                addr: r.addr()?,
                class_of_device: r.u24()?,
                link_type: r.u8()?,
            },
            op::EV_CONNECTION_COMPLETE => ConnectionComplete {
                status: r.u8()?,
                handle: r.u16()?,
                addr: r.addr()?,
                link_type: r.u8()?,
                encryption_enabled: r.u8()? != 0,
            },
            op::EV_DISCONNECTION_COMPLETE => DisconnectionComplete {
                status: r.u8()?,
                handle: r.u16()?,
                reason: r.u8()?,
            },
            op::EV_AUTHENTICATION_COMPLETE => AuthenticationComplete {
                status: r.u8()?,
                handle: r.u16()?,
            },
            op::EV_ENCRYPTION_CHANGE => EncryptionChange {
                status: r.u8()?,
                handle: r.u16()?,
                enabled: r.u8()? != 0,
            },
            op::EV_NUMBER_OF_COMPLETED_PACKETS => {
                let n = usize::from(r.u8()?);
                // Count and body must agree before anything is allocated.
                if r.remaining() < n * 4 {
                    return Err(Error::Truncated);
                }
                let mut handles = Vec::with_capacity(n);
                for _ in 0..n {
                    handles.push(r.u16()?);
                }
                let mut out = Vec::with_capacity(n);
                for h in handles {
                    out.push((h & 0x0FFF, r.u16()?));
                }
                NumberOfCompletedPackets(out)
            }
            op::EV_LINK_KEY_REQUEST => LinkKeyRequest { addr: r.addr()? },
            op::EV_LINK_KEY_NOTIFICATION => LinkKeyNotification {
                addr: r.addr()?,
                key: r.array()?,
                key_type: r.u8()?,
            },
            op::EV_IO_CAPABILITY_REQUEST => IoCapabilityRequest { addr: r.addr()? },
            op::EV_IO_CAPABILITY_RESPONSE => IoCapabilityResponse {
                addr: r.addr()?,
                io_capability: r.u8()?,
                oob_present: r.u8()?,
                authentication_requirements: r.u8()?,
            },
            op::EV_USER_CONFIRMATION_REQUEST => UserConfirmationRequest {
                addr: r.addr()?,
                numeric_value: r.u32()?,
            },
            op::EV_USER_PASSKEY_REQUEST => UserPasskeyRequest { addr: r.addr()? },
            op::EV_USER_PASSKEY_NOTIFICATION => UserPasskeyNotification {
                addr: r.addr()?,
                passkey: r.u32()?,
            },
            op::EV_SIMPLE_PAIRING_COMPLETE => SimplePairingComplete {
                status: r.u8()?,
                addr: r.addr()?,
            },
            op::EV_LE_META => Event::parse_le(&mut r)?,
            _ => Unknown {
                code,
                params: params.to_vec(),
            },
        })
    }

    fn parse_le(r: &mut Reader<'_>) -> Result<Event, Error> {
        let sub = r.u8()?;
        Ok(match sub {
            op::LE_CONNECTION_COMPLETE => {
                let mut c = LeConnection::parse(r)?;
                c.parse_timing(r)?;
                Event::LeConnectionComplete(c)
            }
            op::LE_ENHANCED_CONNECTION_COMPLETE => {
                let mut conn = LeConnection::parse(r)?;
                let local_rpa = r.addr()?;
                let peer_rpa = r.addr()?;
                conn.parse_timing(r)?;
                Event::LeEnhancedConnectionComplete {
                    conn,
                    local_rpa,
                    peer_rpa,
                }
            }
            op::LE_ADVERTISING_REPORT => {
                let n = usize::from(r.u8()?);
                if n > MAX_ADV_REPORTS {
                    return Err(Error::TooLong);
                }
                let mut reports = Vec::with_capacity(n);
                for _ in 0..n {
                    let event_type = r.u8()?;
                    let address_type = r.u8()?;
                    let addr = r.addr()?;
                    let dlen = usize::from(r.u8()?);
                    if dlen > MAX_ADV_DATA {
                        return Err(Error::TooLong);
                    }
                    let data = r.take(dlen)?.to_vec();
                    reports.push(AdvReport {
                        event_type,
                        address_type,
                        addr,
                        data,
                        rssi: r.u8()? as i8,
                    });
                }
                Event::LeAdvertisingReport(reports)
            }
            op::LE_LONG_TERM_KEY_REQUEST => Event::LeLongTermKeyRequest {
                handle: r.u16()?,
                random: r.u64()?,
                ediv: r.u16()?,
            },
            _ => Event::UnknownLe {
                subevent: sub,
                params: r.rest().to_vec(),
            },
        })
    }
}
