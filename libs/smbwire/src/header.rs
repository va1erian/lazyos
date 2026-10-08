//! The 64-byte SMB2 header (`MS-SMB2` 2.2.1), sync form for requests; a
//! response may come in the async form (an interim `STATUS_PENDING`).

use alloc::vec::Vec;

use crate::{le16, le32, le64, Error};

/// Bytes in a header.
pub const SIZE: usize = 64;
/// `0xFE 'S' 'M' 'B'`.
pub const PROTOCOL_ID: [u8; 4] = [0xFE, b'S', b'M', b'B'];
/// Where the signature sits in a header.
pub const SIGNATURE_AT: usize = 48;

pub const FLAG_RESPONSE: u32 = 0x0000_0001;
pub const FLAG_ASYNC: u32 = 0x0000_0002;
pub const FLAG_RELATED: u32 = 0x0000_0004;
pub const FLAG_SIGNED: u32 = 0x0000_0008;

/// The `MessageId` of an unsolicited oplock break notification.
pub const UNSOLICITED: u64 = u64::MAX;

/// Command codes.
pub mod command {
    pub const NEGOTIATE: u16 = 0x0000;
    pub const SESSION_SETUP: u16 = 0x0001;
    pub const LOGOFF: u16 = 0x0002;
    pub const TREE_CONNECT: u16 = 0x0003;
    pub const TREE_DISCONNECT: u16 = 0x0004;
    pub const CREATE: u16 = 0x0005;
    pub const CLOSE: u16 = 0x0006;
    pub const FLUSH: u16 = 0x0007;
    pub const READ: u16 = 0x0008;
    pub const WRITE: u16 = 0x0009;
    pub const QUERY_DIRECTORY: u16 = 0x000E;
    pub const QUERY_INFO: u16 = 0x0010;
    pub const SET_INFO: u16 = 0x0011;
    pub const OPLOCK_BREAK: u16 = 0x0012;

    /// The command's name for messages.
    pub fn name(code: u16) -> &'static str {
        match code {
            NEGOTIATE => "NEGOTIATE",
            SESSION_SETUP => "SESSION_SETUP",
            LOGOFF => "LOGOFF",
            TREE_CONNECT => "TREE_CONNECT",
            TREE_DISCONNECT => "TREE_DISCONNECT",
            CREATE => "CREATE",
            CLOSE => "CLOSE",
            FLUSH => "FLUSH",
            READ => "READ",
            WRITE => "WRITE",
            QUERY_DIRECTORY => "QUERY_DIRECTORY",
            QUERY_INFO => "QUERY_INFO",
            SET_INFO => "SET_INFO",
            OPLOCK_BREAK => "OPLOCK_BREAK",
            _ => "UNKNOWN",
        }
    }
}

/// The fields of a header the client sets or reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Header {
    pub credit_charge: u16,
    pub status: u32,
    pub command: u16,
    /// `CreditRequest` in a request, `CreditResponse` in a response.
    pub credits: u16,
    pub flags: u32,
    pub next_command: u32,
    pub message_id: u64,
    /// The async id of an async response; zero otherwise.
    pub async_id: u64,
    pub tree_id: u32,
    pub session_id: u64,
    pub signature: [u8; 16],
}

impl Header {
    /// A request header for `command`.
    pub fn request(command: u16, message_id: u64, tree_id: u32, session_id: u64) -> Header {
        Header {
            command,
            message_id,
            tree_id,
            session_id,
            ..Header::default()
        }
    }

    pub fn is_response(&self) -> bool {
        self.flags & FLAG_RESPONSE != 0
    }

    pub fn is_async(&self) -> bool {
        self.flags & FLAG_ASYNC != 0
    }

    pub fn is_signed(&self) -> bool {
        self.flags & FLAG_SIGNED != 0
    }

    /// The header's 64 bytes.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&PROTOCOL_ID);
        out.extend_from_slice(&(SIZE as u16).to_le_bytes());
        out.extend_from_slice(&self.credit_charge.to_le_bytes());
        out.extend_from_slice(&self.status.to_le_bytes());
        out.extend_from_slice(&self.command.to_le_bytes());
        out.extend_from_slice(&self.credits.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.next_command.to_le_bytes());
        out.extend_from_slice(&self.message_id.to_le_bytes());
        if self.is_async() {
            out.extend_from_slice(&self.async_id.to_le_bytes());
        } else {
            // Reserved (the process id), then the tree.
            out.extend_from_slice(&0xFEFFu32.to_le_bytes());
            out.extend_from_slice(&self.tree_id.to_le_bytes());
        }
        out.extend_from_slice(&self.session_id.to_le_bytes());
        out.extend_from_slice(&self.signature);
    }

    /// Parse the header at the start of `message`.
    pub fn parse(message: &[u8]) -> Result<Header, Error> {
        let bad = Error::Malformed("SMB2 header");
        if message.len() < SIZE || message[..4] != PROTOCOL_ID {
            return Err(bad);
        }
        if le16(message, 4) != Some(SIZE as u16) {
            return Err(bad);
        }
        let flags = le32(message, 16).ok_or(bad.clone())?;
        let mut header = Header {
            credit_charge: le16(message, 6).ok_or(bad.clone())?,
            status: le32(message, 8).ok_or(bad.clone())?,
            command: le16(message, 12).ok_or(bad.clone())?,
            credits: le16(message, 14).ok_or(bad.clone())?,
            flags,
            next_command: le32(message, 20).ok_or(bad.clone())?,
            message_id: le64(message, 24).ok_or(bad.clone())?,
            session_id: le64(message, 40).ok_or(bad.clone())?,
            ..Header::default()
        };
        if header.is_async() {
            header.async_id = le64(message, 32).ok_or(bad.clone())?;
        } else {
            header.tree_id = le32(message, 36).ok_or(bad)?;
        }
        header
            .signature
            .copy_from_slice(&message[SIGNATURE_AT..SIZE]);
        Ok(header)
    }
}
