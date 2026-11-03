//! SMP PDUs (Core Specification Vol 3, Part H, 3): one octet of code then a
//! fixed-size body. Every PDU is hostile input: [`Pdu::decode`] checks the
//! exact length of the body (SMP has no variable-length commands) and the
//! ranges of the enumerations, and never panics.
//!
//! Multi-octet fields stay in air order (little-endian); `btsmp::func` is where
//! the specification's most-significant-first form is handled.

use alloc::vec::Vec;

/// Pairing Request/Response code and the other command codes.
pub mod code {
    pub const PAIRING_REQUEST: u8 = 0x01;
    pub const PAIRING_RESPONSE: u8 = 0x02;
    pub const PAIRING_CONFIRM: u8 = 0x03;
    pub const PAIRING_RANDOM: u8 = 0x04;
    pub const PAIRING_FAILED: u8 = 0x05;
    pub const ENCRYPTION_INFO: u8 = 0x06;
    pub const CENTRAL_IDENTIFICATION: u8 = 0x07;
    pub const IDENTITY_INFO: u8 = 0x08;
    pub const IDENTITY_ADDRESS_INFO: u8 = 0x09;
    pub const SIGNING_INFO: u8 = 0x0A;
    pub const SECURITY_REQUEST: u8 = 0x0B;
    pub const PUBLIC_KEY: u8 = 0x0C;
    pub const DHKEY_CHECK: u8 = 0x0D;
    pub const KEYPRESS_NOTIFICATION: u8 = 0x0E;
}

/// The IO capability of a device (Table 3.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IoCapability {
    DisplayOnly = 0,
    DisplayYesNo = 1,
    KeyboardOnly = 2,
    NoInputNoOutput = 3,
    KeyboardDisplay = 4,
}

impl IoCapability {
    fn from_byte(byte: u8) -> Option<IoCapability> {
        Some(match byte {
            0 => IoCapability::DisplayOnly,
            1 => IoCapability::DisplayYesNo,
            2 => IoCapability::KeyboardOnly,
            3 => IoCapability::NoInputNoOutput,
            4 => IoCapability::KeyboardDisplay,
            _ => return None,
        })
    }
}

/// The AuthReq octet (Figure 3.3). The raw value is kept because `f6` hashes
/// it exactly as it was sent, reserved bits included.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AuthReq(pub u8);

impl AuthReq {
    pub const BONDING: u8 = 0x01;
    pub const MITM: u8 = 0x04;
    pub const SECURE_CONNECTIONS: u8 = 0x08;
    pub const KEYPRESS: u8 = 0x10;
    pub const CT2: u8 = 0x20;

    pub fn bonding(self) -> bool {
        self.0 & 0x03 == 1
    }
    pub fn mitm(self) -> bool {
        self.0 & Self::MITM != 0
    }
    pub fn secure_connections(self) -> bool {
        self.0 & Self::SECURE_CONNECTIONS != 0
    }
}

/// Key distribution flags (Figure 3.5). Only `ID_KEY` is used in LE SC.
pub mod keydist {
    pub const ENC_KEY: u8 = 0x01;
    pub const ID_KEY: u8 = 0x02;
    pub const SIGN_KEY: u8 = 0x04;
    pub const LINK_KEY: u8 = 0x08;
}

/// The body of a Pairing Request or Response.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PairingFeatures {
    pub io_capability: IoCapability,
    pub oob_present: bool,
    pub auth_req: AuthReq,
    /// Maximum encryption key size, 7..=16 octets.
    pub max_key_size: u8,
    pub initiator_keys: u8,
    pub responder_keys: u8,
}

impl PairingFeatures {
    /// The three octets `f6` takes as `IOcap`: `AuthReq || OOB || IO`.
    pub fn io_cap_bytes(&self) -> [u8; 3] {
        [
            self.auth_req.0,
            self.oob_present as u8,
            self.io_capability as u8,
        ]
    }
}

/// A Pairing Failed reason (Table 3.7). Unknown values are kept, not refused:
/// a peer may use a code newer than this table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reason(pub u8);

impl Reason {
    pub const PASSKEY_ENTRY_FAILED: Reason = Reason(0x01);
    pub const OOB_NOT_AVAILABLE: Reason = Reason(0x02);
    pub const AUTHENTICATION_REQUIREMENTS: Reason = Reason(0x03);
    pub const CONFIRM_VALUE_FAILED: Reason = Reason(0x04);
    pub const PAIRING_NOT_SUPPORTED: Reason = Reason(0x05);
    pub const ENCRYPTION_KEY_SIZE: Reason = Reason(0x06);
    pub const COMMAND_NOT_SUPPORTED: Reason = Reason(0x07);
    pub const UNSPECIFIED: Reason = Reason(0x08);
    pub const INVALID_PARAMETERS: Reason = Reason(0x0A);
    pub const DHKEY_CHECK_FAILED: Reason = Reason(0x0B);
    pub const NUMERIC_COMPARISON_FAILED: Reason = Reason(0x0C);
}

/// The type of an Identity Address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddrType {
    Public = 0,
    Random = 1,
}

/// A decoded SMP PDU.
///
/// `Debug` prints the command name only: Encryption/Identity/Signing
/// Information carry keys and the others carry values an attacker could use
/// while a pairing is open.
#[derive(Clone, PartialEq, Eq)]
pub enum Pdu {
    PairingRequest(PairingFeatures),
    PairingResponse(PairingFeatures),
    PairingConfirm([u8; 16]),
    PairingRandom([u8; 16]),
    PairingFailed(Reason),
    /// Legacy LTK; never sent in LE SC, decoded so it can be refused by name.
    EncryptionInfo([u8; 16]),
    CentralIdentification {
        ediv: u16,
        rand: [u8; 8],
    },
    IdentityInfo([u8; 16]),
    IdentityAddressInfo {
        addr_type: AddrType,
        addr: [u8; 6],
    },
    SigningInfo([u8; 16]),
    SecurityRequest(AuthReq),
    /// `X || Y`, each little-endian.
    PublicKey([u8; 64]),
    DhKeyCheck([u8; 16]),
    KeypressNotification(u8),
}

/// Why bytes are not a PDU.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecodeError {
    /// Zero octets.
    Empty,
    /// A command code this table does not define.
    UnknownCode(u8),
    /// The body is not the size the command has.
    BadLength { code: u8 },
    /// A field holds a value outside its range.
    BadValue { code: u8 },
}

impl DecodeError {
    /// The Pairing Failed reason a receiver answers with.
    pub fn reason(self) -> Reason {
        match self {
            DecodeError::UnknownCode(_) => Reason::COMMAND_NOT_SUPPORTED,
            _ => Reason::INVALID_PARAMETERS,
        }
    }
}

impl core::fmt::Debug for Pdu {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

fn take<const N: usize>(body: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    out.copy_from_slice(body);
    out
}

fn features(code: u8, body: &[u8]) -> Result<PairingFeatures, DecodeError> {
    let bad = DecodeError::BadValue { code };
    Ok(PairingFeatures {
        io_capability: IoCapability::from_byte(body[0]).ok_or(bad)?,
        oob_present: match body[1] {
            0 => false,
            1 => true,
            _ => return Err(bad),
        },
        auth_req: AuthReq(body[2]),
        max_key_size: match body[3] {
            size @ 7..=16 => size,
            _ => return Err(bad),
        },
        initiator_keys: body[4],
        responder_keys: body[5],
    })
}

impl Pdu {
    /// The command's name, for traces.
    pub fn name(&self) -> &'static str {
        match self {
            Pdu::PairingRequest(_) => "PairingRequest",
            Pdu::PairingResponse(_) => "PairingResponse",
            Pdu::PairingConfirm(_) => "PairingConfirm",
            Pdu::PairingRandom(_) => "PairingRandom",
            Pdu::PairingFailed(_) => "PairingFailed",
            Pdu::EncryptionInfo(_) => "EncryptionInformation",
            Pdu::CentralIdentification { .. } => "CentralIdentification",
            Pdu::IdentityInfo(_) => "IdentityInformation",
            Pdu::IdentityAddressInfo { .. } => "IdentityAddressInformation",
            Pdu::SigningInfo(_) => "SigningInformation",
            Pdu::SecurityRequest(_) => "SecurityRequest",
            Pdu::PublicKey(_) => "PublicKey",
            Pdu::DhKeyCheck(_) => "DhKeyCheck",
            Pdu::KeypressNotification(_) => "KeypressNotification",
        }
    }

    /// The command code on the air.
    pub fn code(&self) -> u8 {
        match self {
            Pdu::PairingRequest(_) => code::PAIRING_REQUEST,
            Pdu::PairingResponse(_) => code::PAIRING_RESPONSE,
            Pdu::PairingConfirm(_) => code::PAIRING_CONFIRM,
            Pdu::PairingRandom(_) => code::PAIRING_RANDOM,
            Pdu::PairingFailed(_) => code::PAIRING_FAILED,
            Pdu::EncryptionInfo(_) => code::ENCRYPTION_INFO,
            Pdu::CentralIdentification { .. } => code::CENTRAL_IDENTIFICATION,
            Pdu::IdentityInfo(_) => code::IDENTITY_INFO,
            Pdu::IdentityAddressInfo { .. } => code::IDENTITY_ADDRESS_INFO,
            Pdu::SigningInfo(_) => code::SIGNING_INFO,
            Pdu::SecurityRequest(_) => code::SECURITY_REQUEST,
            Pdu::PublicKey(_) => code::PUBLIC_KEY,
            Pdu::DhKeyCheck(_) => code::DHKEY_CHECK,
            Pdu::KeypressNotification(_) => code::KEYPRESS_NOTIFICATION,
        }
    }

    /// Parse one PDU (the L2CAP SMP channel payload).
    pub fn decode(bytes: &[u8]) -> Result<Pdu, DecodeError> {
        let (&command, body) = bytes.split_first().ok_or(DecodeError::Empty)?;
        let expect = |len: usize| {
            if body.len() == len {
                Ok(())
            } else {
                Err(DecodeError::BadLength { code: command })
            }
        };
        Ok(match command {
            code::PAIRING_REQUEST => {
                expect(6)?;
                Pdu::PairingRequest(features(command, body)?)
            }
            code::PAIRING_RESPONSE => {
                expect(6)?;
                Pdu::PairingResponse(features(command, body)?)
            }
            code::PAIRING_CONFIRM => {
                expect(16)?;
                Pdu::PairingConfirm(take(body))
            }
            code::PAIRING_RANDOM => {
                expect(16)?;
                Pdu::PairingRandom(take(body))
            }
            code::PAIRING_FAILED => {
                expect(1)?;
                Pdu::PairingFailed(Reason(body[0]))
            }
            code::ENCRYPTION_INFO => {
                expect(16)?;
                Pdu::EncryptionInfo(take(body))
            }
            code::CENTRAL_IDENTIFICATION => {
                expect(10)?;
                Pdu::CentralIdentification {
                    ediv: u16::from_le_bytes([body[0], body[1]]),
                    rand: take(&body[2..]),
                }
            }
            code::IDENTITY_INFO => {
                expect(16)?;
                Pdu::IdentityInfo(take(body))
            }
            code::IDENTITY_ADDRESS_INFO => {
                expect(7)?;
                let addr_type = match body[0] {
                    0 => AddrType::Public,
                    1 => AddrType::Random,
                    _ => return Err(DecodeError::BadValue { code: command }),
                };
                Pdu::IdentityAddressInfo {
                    addr_type,
                    addr: take(&body[1..]),
                }
            }
            code::SIGNING_INFO => {
                expect(16)?;
                Pdu::SigningInfo(take(body))
            }
            code::SECURITY_REQUEST => {
                expect(1)?;
                Pdu::SecurityRequest(AuthReq(body[0]))
            }
            code::PUBLIC_KEY => {
                expect(64)?;
                Pdu::PublicKey(take(body))
            }
            code::DHKEY_CHECK => {
                expect(16)?;
                Pdu::DhKeyCheck(take(body))
            }
            code::KEYPRESS_NOTIFICATION => {
                expect(1)?;
                Pdu::KeypressNotification(body[0])
            }
            other => return Err(DecodeError::UnknownCode(other)),
        })
    }

    /// The PDU as it goes on the air.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(65);
        out.push(self.code());
        match self {
            Pdu::PairingRequest(f) | Pdu::PairingResponse(f) => out.extend_from_slice(&[
                f.io_capability as u8,
                f.oob_present as u8,
                f.auth_req.0,
                f.max_key_size,
                f.initiator_keys,
                f.responder_keys,
            ]),
            Pdu::PairingConfirm(v)
            | Pdu::PairingRandom(v)
            | Pdu::EncryptionInfo(v)
            | Pdu::IdentityInfo(v)
            | Pdu::SigningInfo(v)
            | Pdu::DhKeyCheck(v) => out.extend_from_slice(v),
            Pdu::PairingFailed(reason) => out.push(reason.0),
            Pdu::CentralIdentification { ediv, rand } => {
                out.extend_from_slice(&ediv.to_le_bytes());
                out.extend_from_slice(rand);
            }
            Pdu::IdentityAddressInfo { addr_type, addr } => {
                out.push(*addr_type as u8);
                out.extend_from_slice(addr);
            }
            Pdu::SecurityRequest(auth) => out.push(auth.0),
            Pdu::PublicKey(key) => out.extend_from_slice(key),
            Pdu::KeypressNotification(kind) => out.push(*kind),
        }
        out
    }
}
