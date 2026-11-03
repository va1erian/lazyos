//! HCI command packets: what the host sends, and (for the simulator and tests)
//! what a controller parses.
//!
//! A command packet is `opcode:u16, length:u8, parameters`. No transport
//! indicator byte: the USB transport (H2) carries the packet as a control
//! request, so the codec stops at the HCI packet.

use alloc::vec::Vec;

use crate::addr::BdAddr;
use crate::opcode as op;
use crate::wire::{put_u16, put_u24, put_u32, put_u64, Reader};
use crate::Error;

/// The longest local name Write_Local_Name carries.
pub const NAME_LEN: usize = 248;
/// Largest parameter block an HCI command can carry.
pub const MAX_PARAMS: usize = 255;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Reset,
    ReadBdAddr,
    ReadLocalVersion,
    ReadBufferSize,
    LeReadBufferSize,
    SetEventMask(u64),
    LeSetEventMask(u64),
    /// Bit 0 inquiry scan, bit 1 page scan.
    WriteScanEnable(u8),
    WriteSimplePairingMode(bool),
    /// 24-bit class of device.
    WriteClassOfDevice(u32),
    /// Up to [`NAME_LEN`] octets; shorter names are zero padded on the wire.
    WriteLocalName(Vec<u8>),
    CreateConnection {
        addr: BdAddr,
        packet_type: u16,
        page_scan_repetition_mode: u8,
        clock_offset: u16,
        allow_role_switch: u8,
    },
    AcceptConnectionRequest {
        addr: BdAddr,
        role: u8,
    },
    RejectConnectionRequest {
        addr: BdAddr,
        reason: u8,
    },
    AuthenticationRequested {
        handle: u16,
    },
    SetConnectionEncryption {
        handle: u16,
        enable: bool,
    },
    LinkKeyRequestReply {
        addr: BdAddr,
        key: [u8; 16],
    },
    LinkKeyRequestNegativeReply {
        addr: BdAddr,
    },
    IoCapabilityRequestReply {
        addr: BdAddr,
        io_capability: u8,
        oob_present: u8,
        authentication_requirements: u8,
    },
    IoCapabilityRequestNegativeReply {
        addr: BdAddr,
        reason: u8,
    },
    UserConfirmationRequestReply {
        addr: BdAddr,
    },
    UserConfirmationRequestNegativeReply {
        addr: BdAddr,
    },
    UserPasskeyRequestReply {
        addr: BdAddr,
        passkey: u32,
    },
    UserPasskeyRequestNegativeReply {
        addr: BdAddr,
    },
    Disconnect {
        handle: u16,
        reason: u8,
    },
    /// Status parameters group: the negotiated encryption key size of a
    /// link (KNOB: it can be as short as one octet).
    ReadEncryptionKeySize {
        handle: u16,
    },
    LeSetScanParameters {
        active: bool,
        interval: u16,
        window: u16,
        own_address_type: u8,
        filter_policy: u8,
    },
    LeSetScanEnable {
        enable: bool,
        filter_duplicates: bool,
    },
    LeCreateConnection {
        scan_interval: u16,
        scan_window: u16,
        /// 0: connect to `peer`; 1: connect to anything on the accept list.
        filter_policy: u8,
        peer_address_type: u8,
        peer: BdAddr,
        own_address_type: u8,
        interval_min: u16,
        interval_max: u16,
        latency: u16,
        supervision_timeout: u16,
        min_ce_length: u16,
        max_ce_length: u16,
    },
    LeCreateConnectionCancel,
    LeClearAcceptList,
    LeAddDeviceToAcceptList {
        address_type: u8,
        addr: BdAddr,
    },
    LeStartEncryption {
        handle: u16,
        random: u64,
        ediv: u16,
        ltk: [u8; 16],
    },
    LeLongTermKeyRequestReply {
        handle: u16,
        ltk: [u8; 16],
    },
    LeLongTermKeyRequestNegativeReply {
        handle: u16,
    },
    /// An opcode this codec has no layout for (parameters kept verbatim).
    Unknown {
        opcode: u16,
        params: Vec<u8>,
    },
}

impl Command {
    pub fn opcode(&self) -> u16 {
        use Command::*;
        match self {
            Reset => op::RESET,
            ReadBdAddr => op::READ_BD_ADDR,
            ReadLocalVersion => op::READ_LOCAL_VERSION,
            ReadBufferSize => op::READ_BUFFER_SIZE,
            LeReadBufferSize => op::LE_READ_BUFFER_SIZE,
            SetEventMask(_) => op::SET_EVENT_MASK,
            LeSetEventMask(_) => op::LE_SET_EVENT_MASK,
            WriteScanEnable(_) => op::WRITE_SCAN_ENABLE,
            WriteSimplePairingMode(_) => op::WRITE_SIMPLE_PAIRING_MODE,
            WriteClassOfDevice(_) => op::WRITE_CLASS_OF_DEVICE,
            WriteLocalName(_) => op::WRITE_LOCAL_NAME,
            CreateConnection { .. } => op::CREATE_CONNECTION,
            AcceptConnectionRequest { .. } => op::ACCEPT_CONNECTION_REQUEST,
            RejectConnectionRequest { .. } => op::REJECT_CONNECTION_REQUEST,
            AuthenticationRequested { .. } => op::AUTHENTICATION_REQUESTED,
            SetConnectionEncryption { .. } => op::SET_CONNECTION_ENCRYPTION,
            LinkKeyRequestReply { .. } => op::LINK_KEY_REQUEST_REPLY,
            LinkKeyRequestNegativeReply { .. } => op::LINK_KEY_REQUEST_NEGATIVE_REPLY,
            IoCapabilityRequestReply { .. } => op::IO_CAPABILITY_REQUEST_REPLY,
            IoCapabilityRequestNegativeReply { .. } => op::IO_CAPABILITY_REQUEST_NEGATIVE_REPLY,
            UserConfirmationRequestReply { .. } => op::USER_CONFIRMATION_REQUEST_REPLY,
            UserConfirmationRequestNegativeReply { .. } => {
                op::USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY
            }
            UserPasskeyRequestReply { .. } => op::USER_PASSKEY_REQUEST_REPLY,
            UserPasskeyRequestNegativeReply { .. } => op::USER_PASSKEY_REQUEST_NEGATIVE_REPLY,
            Disconnect { .. } => op::DISCONNECT,
            ReadEncryptionKeySize { .. } => op::READ_ENCRYPTION_KEY_SIZE,
            LeSetScanParameters { .. } => op::LE_SET_SCAN_PARAMETERS,
            LeSetScanEnable { .. } => op::LE_SET_SCAN_ENABLE,
            LeCreateConnection { .. } => op::LE_CREATE_CONNECTION,
            LeCreateConnectionCancel => op::LE_CREATE_CONNECTION_CANCEL,
            LeClearAcceptList => op::LE_CLEAR_ACCEPT_LIST,
            LeAddDeviceToAcceptList { .. } => op::LE_ADD_DEVICE_TO_ACCEPT_LIST,
            LeStartEncryption { .. } => op::LE_START_ENCRYPTION,
            LeLongTermKeyRequestReply { .. } => op::LE_LONG_TERM_KEY_REQUEST_REPLY,
            LeLongTermKeyRequestNegativeReply { .. } => op::LE_LONG_TERM_KEY_REQUEST_NEGATIVE_REPLY,
            Unknown { opcode, .. } => *opcode,
        }
    }

    /// The complete command packet. A name or unknown-parameter block longer
    /// than the wire allows is cut to fit rather than overflowing the length
    /// octet.
    pub fn encode(&self) -> Vec<u8> {
        let mut params = Vec::new();
        self.put_params(&mut params);
        params.truncate(MAX_PARAMS);
        let mut out = Vec::with_capacity(3 + params.len());
        put_u16(&mut out, self.opcode());
        out.push(params.len() as u8);
        out.extend_from_slice(&params);
        out
    }

    fn put_params(&self, p: &mut Vec<u8>) {
        use Command::*;
        match self {
            Reset
            | ReadBdAddr
            | ReadLocalVersion
            | ReadBufferSize
            | LeReadBufferSize
            | LeCreateConnectionCancel
            | LeClearAcceptList => {}
            SetEventMask(m) | LeSetEventMask(m) => put_u64(p, *m),
            WriteScanEnable(v) => p.push(*v),
            WriteSimplePairingMode(v) => p.push(u8::from(*v)),
            WriteClassOfDevice(c) => put_u24(p, *c),
            WriteLocalName(name) => {
                let n = name.len().min(NAME_LEN);
                p.extend_from_slice(&name[..n]);
                p.resize(NAME_LEN, 0);
            }
            CreateConnection {
                addr,
                packet_type,
                page_scan_repetition_mode,
                clock_offset,
                allow_role_switch,
            } => {
                p.extend_from_slice(&addr.0);
                put_u16(p, *packet_type);
                p.push(*page_scan_repetition_mode);
                p.push(0); // reserved
                put_u16(p, *clock_offset);
                p.push(*allow_role_switch);
            }
            AcceptConnectionRequest { addr, role } => {
                p.extend_from_slice(&addr.0);
                p.push(*role);
            }
            RejectConnectionRequest { addr, reason }
            | IoCapabilityRequestNegativeReply { addr, reason } => {
                p.extend_from_slice(&addr.0);
                p.push(*reason);
            }
            AuthenticationRequested { handle }
            | ReadEncryptionKeySize { handle }
            | LeLongTermKeyRequestNegativeReply { handle } => put_u16(p, *handle),
            SetConnectionEncryption { handle, enable } => {
                put_u16(p, *handle);
                p.push(u8::from(*enable));
            }
            LinkKeyRequestReply { addr, key } => {
                p.extend_from_slice(&addr.0);
                p.extend_from_slice(key);
            }
            LinkKeyRequestNegativeReply { addr }
            | UserConfirmationRequestReply { addr }
            | UserConfirmationRequestNegativeReply { addr }
            | UserPasskeyRequestNegativeReply { addr } => p.extend_from_slice(&addr.0),
            IoCapabilityRequestReply {
                addr,
                io_capability,
                oob_present,
                authentication_requirements,
            } => {
                p.extend_from_slice(&addr.0);
                p.extend_from_slice(&[*io_capability, *oob_present, *authentication_requirements]);
            }
            UserPasskeyRequestReply { addr, passkey } => {
                p.extend_from_slice(&addr.0);
                put_u32(p, *passkey);
            }
            Disconnect { handle, reason } => {
                put_u16(p, *handle);
                p.push(*reason);
            }
            LeSetScanParameters {
                active,
                interval,
                window,
                own_address_type,
                filter_policy,
            } => {
                p.push(u8::from(*active));
                put_u16(p, *interval);
                put_u16(p, *window);
                p.extend_from_slice(&[*own_address_type, *filter_policy]);
            }
            LeSetScanEnable {
                enable,
                filter_duplicates,
            } => p.extend_from_slice(&[u8::from(*enable), u8::from(*filter_duplicates)]),
            LeCreateConnection {
                scan_interval,
                scan_window,
                filter_policy,
                peer_address_type,
                peer,
                own_address_type,
                interval_min,
                interval_max,
                latency,
                supervision_timeout,
                min_ce_length,
                max_ce_length,
            } => {
                put_u16(p, *scan_interval);
                put_u16(p, *scan_window);
                p.extend_from_slice(&[*filter_policy, *peer_address_type]);
                p.extend_from_slice(&peer.0);
                p.push(*own_address_type);
                for v in [
                    interval_min,
                    interval_max,
                    latency,
                    supervision_timeout,
                    min_ce_length,
                    max_ce_length,
                ] {
                    put_u16(p, *v);
                }
            }
            LeAddDeviceToAcceptList { address_type, addr } => {
                p.push(*address_type);
                p.extend_from_slice(&addr.0);
            }
            LeStartEncryption {
                handle,
                random,
                ediv,
                ltk,
            } => {
                put_u16(p, *handle);
                put_u64(p, *random);
                put_u16(p, *ediv);
                p.extend_from_slice(ltk);
            }
            LeLongTermKeyRequestReply { handle, ltk } => {
                put_u16(p, *handle);
                p.extend_from_slice(ltk);
            }
            Unknown { params, .. } => p.extend_from_slice(params),
        }
    }

    /// Parse a complete command packet. The length octet must match the
    /// packet and the layout of the opcode exactly.
    pub fn parse(packet: &[u8]) -> Result<Command, Error> {
        let mut r = Reader::new(packet);
        let opcode = r.u16()?;
        let len = usize::from(r.u8()?);
        if r.remaining() != len {
            return Err(Error::BadLength);
        }
        Command::parse_params(opcode, r.rest())
    }

    fn parse_params(opcode: u16, params: &[u8]) -> Result<Command, Error> {
        use Command::*;
        let mut r = Reader::new(params);
        let cmd = match opcode {
            op::RESET => Reset,
            op::READ_BD_ADDR => ReadBdAddr,
            op::READ_LOCAL_VERSION => ReadLocalVersion,
            op::READ_BUFFER_SIZE => ReadBufferSize,
            op::LE_READ_BUFFER_SIZE => LeReadBufferSize,
            op::LE_CREATE_CONNECTION_CANCEL => LeCreateConnectionCancel,
            op::LE_CLEAR_ACCEPT_LIST => LeClearAcceptList,
            op::SET_EVENT_MASK => SetEventMask(r.u64()?),
            op::LE_SET_EVENT_MASK => LeSetEventMask(r.u64()?),
            op::WRITE_SCAN_ENABLE => WriteScanEnable(r.u8()?),
            op::WRITE_SIMPLE_PAIRING_MODE => WriteSimplePairingMode(r.u8()? != 0),
            op::WRITE_CLASS_OF_DEVICE => WriteClassOfDevice(r.u24()?),
            op::WRITE_LOCAL_NAME => {
                let raw = r.take(NAME_LEN)?;
                let end = raw.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
                WriteLocalName(raw[..end].to_vec())
            }
            op::CREATE_CONNECTION => {
                let addr = r.addr()?;
                let packet_type = r.u16()?;
                let page_scan_repetition_mode = r.u8()?;
                r.u8()?;
                CreateConnection {
                    addr,
                    packet_type,
                    page_scan_repetition_mode,
                    clock_offset: r.u16()?,
                    allow_role_switch: r.u8()?,
                }
            }
            op::ACCEPT_CONNECTION_REQUEST => AcceptConnectionRequest {
                addr: r.addr()?,
                role: r.u8()?,
            },
            op::REJECT_CONNECTION_REQUEST => RejectConnectionRequest {
                addr: r.addr()?,
                reason: r.u8()?,
            },
            op::IO_CAPABILITY_REQUEST_NEGATIVE_REPLY => IoCapabilityRequestNegativeReply {
                addr: r.addr()?,
                reason: r.u8()?,
            },
            op::AUTHENTICATION_REQUESTED => AuthenticationRequested { handle: r.u16()? },
            op::READ_ENCRYPTION_KEY_SIZE => ReadEncryptionKeySize { handle: r.u16()? },
            op::LE_LONG_TERM_KEY_REQUEST_NEGATIVE_REPLY => {
                LeLongTermKeyRequestNegativeReply { handle: r.u16()? }
            }
            op::SET_CONNECTION_ENCRYPTION => SetConnectionEncryption {
                handle: r.u16()?,
                enable: r.u8()? != 0,
            },
            op::LINK_KEY_REQUEST_REPLY => LinkKeyRequestReply {
                addr: r.addr()?,
                key: r.array()?,
            },
            op::LINK_KEY_REQUEST_NEGATIVE_REPLY => LinkKeyRequestNegativeReply { addr: r.addr()? },
            op::USER_CONFIRMATION_REQUEST_REPLY => UserConfirmationRequestReply { addr: r.addr()? },
            op::USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY => {
                UserConfirmationRequestNegativeReply { addr: r.addr()? }
            }
            op::USER_PASSKEY_REQUEST_NEGATIVE_REPLY => {
                UserPasskeyRequestNegativeReply { addr: r.addr()? }
            }
            op::IO_CAPABILITY_REQUEST_REPLY => IoCapabilityRequestReply {
                addr: r.addr()?,
                io_capability: r.u8()?,
                oob_present: r.u8()?,
                authentication_requirements: r.u8()?,
            },
            op::USER_PASSKEY_REQUEST_REPLY => UserPasskeyRequestReply {
                addr: r.addr()?,
                passkey: r.u32()?,
            },
            op::DISCONNECT => Disconnect {
                handle: r.u16()?,
                reason: r.u8()?,
            },
            op::LE_SET_SCAN_PARAMETERS => LeSetScanParameters {
                active: r.u8()? != 0,
                interval: r.u16()?,
                window: r.u16()?,
                own_address_type: r.u8()?,
                filter_policy: r.u8()?,
            },
            op::LE_SET_SCAN_ENABLE => LeSetScanEnable {
                enable: r.u8()? != 0,
                filter_duplicates: r.u8()? != 0,
            },
            op::LE_CREATE_CONNECTION => LeCreateConnection {
                scan_interval: r.u16()?,
                scan_window: r.u16()?,
                filter_policy: r.u8()?,
                peer_address_type: r.u8()?,
                peer: r.addr()?,
                own_address_type: r.u8()?,
                interval_min: r.u16()?,
                interval_max: r.u16()?,
                latency: r.u16()?,
                supervision_timeout: r.u16()?,
                min_ce_length: r.u16()?,
                max_ce_length: r.u16()?,
            },
            op::LE_ADD_DEVICE_TO_ACCEPT_LIST => LeAddDeviceToAcceptList {
                address_type: r.u8()?,
                addr: r.addr()?,
            },
            op::LE_START_ENCRYPTION => LeStartEncryption {
                handle: r.u16()?,
                random: r.u64()?,
                ediv: r.u16()?,
                ltk: r.array()?,
            },
            op::LE_LONG_TERM_KEY_REQUEST_REPLY => LeLongTermKeyRequestReply {
                handle: r.u16()?,
                ltk: r.array()?,
            },
            _ => {
                return Ok(Unknown {
                    opcode,
                    params: params.to_vec(),
                })
            }
        };
        r.finish()?;
        Ok(cmd)
    }
}
