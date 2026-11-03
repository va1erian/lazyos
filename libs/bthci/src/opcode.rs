//! Opcodes, event codes and status values (Core Specification, Vol 4 Part E).

/// An opcode from its group (OGF) and command (OCF) fields.
pub const fn opcode(ogf: u16, ocf: u16) -> u16 {
    ogf << 10 | ocf
}

pub const OGF_LINK: u16 = 0x01;
pub const OGF_BASEBAND: u16 = 0x03;
pub const OGF_INFO: u16 = 0x04;
pub const OGF_STATUS: u16 = 0x05;
pub const OGF_LE: u16 = 0x08;

pub const CREATE_CONNECTION: u16 = opcode(OGF_LINK, 0x05);
pub const DISCONNECT: u16 = opcode(OGF_LINK, 0x06);
pub const ACCEPT_CONNECTION_REQUEST: u16 = opcode(OGF_LINK, 0x09);
pub const REJECT_CONNECTION_REQUEST: u16 = opcode(OGF_LINK, 0x0A);
pub const LINK_KEY_REQUEST_REPLY: u16 = opcode(OGF_LINK, 0x0B);
pub const LINK_KEY_REQUEST_NEGATIVE_REPLY: u16 = opcode(OGF_LINK, 0x0C);
pub const AUTHENTICATION_REQUESTED: u16 = opcode(OGF_LINK, 0x11);
pub const SET_CONNECTION_ENCRYPTION: u16 = opcode(OGF_LINK, 0x13);
pub const IO_CAPABILITY_REQUEST_REPLY: u16 = opcode(OGF_LINK, 0x2B);
pub const USER_CONFIRMATION_REQUEST_REPLY: u16 = opcode(OGF_LINK, 0x2C);
pub const USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY: u16 = opcode(OGF_LINK, 0x2D);
pub const USER_PASSKEY_REQUEST_REPLY: u16 = opcode(OGF_LINK, 0x2E);
pub const USER_PASSKEY_REQUEST_NEGATIVE_REPLY: u16 = opcode(OGF_LINK, 0x2F);
pub const IO_CAPABILITY_REQUEST_NEGATIVE_REPLY: u16 = opcode(OGF_LINK, 0x34);

pub const SET_EVENT_MASK: u16 = opcode(OGF_BASEBAND, 0x01);
pub const RESET: u16 = opcode(OGF_BASEBAND, 0x03);
pub const WRITE_LOCAL_NAME: u16 = opcode(OGF_BASEBAND, 0x13);
pub const WRITE_SCAN_ENABLE: u16 = opcode(OGF_BASEBAND, 0x1A);
pub const WRITE_CLASS_OF_DEVICE: u16 = opcode(OGF_BASEBAND, 0x24);
pub const WRITE_SIMPLE_PAIRING_MODE: u16 = opcode(OGF_BASEBAND, 0x56);

pub const READ_LOCAL_VERSION: u16 = opcode(OGF_INFO, 0x01);
pub const READ_BUFFER_SIZE: u16 = opcode(OGF_INFO, 0x05);
pub const READ_BD_ADDR: u16 = opcode(OGF_INFO, 0x09);

pub const READ_ENCRYPTION_KEY_SIZE: u16 = opcode(OGF_STATUS, 0x08);

pub const LE_SET_EVENT_MASK: u16 = opcode(OGF_LE, 0x01);
pub const LE_READ_BUFFER_SIZE: u16 = opcode(OGF_LE, 0x02);
pub const LE_SET_SCAN_PARAMETERS: u16 = opcode(OGF_LE, 0x0B);
pub const LE_SET_SCAN_ENABLE: u16 = opcode(OGF_LE, 0x0C);
pub const LE_CREATE_CONNECTION: u16 = opcode(OGF_LE, 0x0D);
pub const LE_CREATE_CONNECTION_CANCEL: u16 = opcode(OGF_LE, 0x0E);
pub const LE_CLEAR_ACCEPT_LIST: u16 = opcode(OGF_LE, 0x10);
pub const LE_ADD_DEVICE_TO_ACCEPT_LIST: u16 = opcode(OGF_LE, 0x11);
pub const LE_START_ENCRYPTION: u16 = opcode(OGF_LE, 0x19);
pub const LE_LONG_TERM_KEY_REQUEST_REPLY: u16 = opcode(OGF_LE, 0x1A);
pub const LE_LONG_TERM_KEY_REQUEST_NEGATIVE_REPLY: u16 = opcode(OGF_LE, 0x1B);

/// Harness builds only (`trace=1`): tell the controller the passkey the host
/// shows, which the simulator hands to its LE keyboard as "what the user
/// typed". A vendor-specific opcode (group 0x3F); a real controller answers
/// Unknown Command, and a normal build never sends it.
pub const HARNESS_SHOW_PASSKEY: u16 = opcode(0x3F, 0x4C);

pub const EV_CONNECTION_COMPLETE: u8 = 0x03;
pub const EV_CONNECTION_REQUEST: u8 = 0x04;
pub const EV_DISCONNECTION_COMPLETE: u8 = 0x05;
pub const EV_AUTHENTICATION_COMPLETE: u8 = 0x06;
pub const EV_ENCRYPTION_CHANGE: u8 = 0x08;
pub const EV_COMMAND_COMPLETE: u8 = 0x0E;
pub const EV_COMMAND_STATUS: u8 = 0x0F;
pub const EV_NUMBER_OF_COMPLETED_PACKETS: u8 = 0x13;
pub const EV_LINK_KEY_REQUEST: u8 = 0x17;
pub const EV_LINK_KEY_NOTIFICATION: u8 = 0x18;
pub const EV_IO_CAPABILITY_REQUEST: u8 = 0x31;
pub const EV_IO_CAPABILITY_RESPONSE: u8 = 0x32;
pub const EV_USER_CONFIRMATION_REQUEST: u8 = 0x33;
pub const EV_USER_PASSKEY_REQUEST: u8 = 0x34;
pub const EV_SIMPLE_PAIRING_COMPLETE: u8 = 0x36;
pub const EV_USER_PASSKEY_NOTIFICATION: u8 = 0x3B;
pub const EV_LE_META: u8 = 0x3E;

pub const LE_CONNECTION_COMPLETE: u8 = 0x01;
pub const LE_ADVERTISING_REPORT: u8 = 0x02;
pub const LE_LONG_TERM_KEY_REQUEST: u8 = 0x05;
pub const LE_ENHANCED_CONNECTION_COMPLETE: u8 = 0x0A;

/// HCI status values this stack names; everything else is just a number.
pub mod status {
    pub const SUCCESS: u8 = 0x00;
    pub const UNKNOWN_COMMAND: u8 = 0x01;
    pub const UNKNOWN_CONNECTION: u8 = 0x02;
    pub const PAGE_TIMEOUT: u8 = 0x04;
    pub const AUTHENTICATION_FAILURE: u8 = 0x05;
    pub const PIN_OR_KEY_MISSING: u8 = 0x06;
    pub const COMMAND_DISALLOWED: u8 = 0x0C;
    pub const CONNECTION_REJECTED_RESOURCES: u8 = 0x0D;
    pub const CONNECTION_REJECTED_SECURITY: u8 = 0x0E;
    pub const INVALID_PARAMETERS: u8 = 0x12;
    pub const REMOTE_USER_TERMINATED: u8 = 0x13;
    pub const CONNECTION_TERMINATED_BY_LOCAL_HOST: u8 = 0x16;
    pub const PAIRING_NOT_ALLOWED: u8 = 0x18;
}

/// The event mask for everything this stack reads: bit n enables event
/// code n + 1, so events 0x01..=0x3E are on, which takes in the classic link
/// events, the Secure Simple Pairing group and the LE Meta event.
pub const EVENT_MASK_HID_HOST: u64 = (1u64 << 62) - 1;

/// The LE event mask: connection complete (bit 0), advertising report (1),
/// long term key request (4) and enhanced connection complete (9).
pub const LE_EVENT_MASK_HID_HOST: u64 = 0x0000_0000_0000_0213;
