//! NEGOTIATE, SESSION_SETUP and TREE_CONNECT.

use alloc::vec::Vec;

use super::{body, buffer};
use crate::header::SIZE as HEADER;
use crate::{le16, le32, le64, Error};

pub const DIALECT_202: u16 = 0x0202;
pub const DIALECT_21: u16 = 0x0210;
/// What a server answers a dialect it cannot pick from (SMB 2 wildcard).
pub const DIALECT_WILDCARD: u16 = 0x02FF;

pub const SIGNING_ENABLED: u16 = 0x0001;
pub const SIGNING_REQUIRED: u16 = 0x0002;

pub const SESSION_FLAG_IS_GUEST: u16 = 0x0001;
pub const SESSION_FLAG_IS_NULL: u16 = 0x0002;
pub const SESSION_FLAG_ENCRYPT_DATA: u16 = 0x0004;

pub const SHARE_TYPE_DISK: u8 = 1;
pub const SHARE_TYPE_PIPE: u8 = 2;
pub const SHARE_TYPE_PRINT: u8 = 3;
pub const SHAREFLAG_ENCRYPT_DATA: u32 = 0x0000_8000;

/// The NEGOTIATE request body.
pub fn negotiate_request(security_mode: u16, client_guid: &[u8; 16], dialects: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(36 + 2 * dialects.len());
    out.extend_from_slice(&36u16.to_le_bytes());
    out.extend_from_slice(&(dialects.len() as u16).to_le_bytes());
    out.extend_from_slice(&security_mode.to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    // No capabilities: no DFS, leasing, large MTU or multichannel.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(client_guid);
    out.extend_from_slice(&0u64.to_le_bytes());
    for dialect in dialects {
        out.extend_from_slice(&dialect.to_le_bytes());
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegotiateResponse {
    pub security_mode: u16,
    pub dialect: u16,
    pub server_guid: [u8; 16],
    pub capabilities: u32,
    pub max_transact: u32,
    pub max_read: u32,
    pub max_write: u32,
    /// The server's clock, FILETIME.
    pub system_time: u64,
    pub security_buffer: Vec<u8>,
}

pub fn parse_negotiate(message: &[u8]) -> Result<NegotiateResponse, Error> {
    const WHAT: &str = "NEGOTIATE response";
    let b = body(message, 65, WHAT)?;
    let bad = Error::Malformed(WHAT);
    let mut server_guid = [0u8; 16];
    server_guid.copy_from_slice(&b[8..24]);
    let offset = le16(b, 56).ok_or(bad.clone())? as usize;
    let len = le16(b, 58).ok_or(bad.clone())? as usize;
    Ok(NegotiateResponse {
        security_mode: le16(b, 2).ok_or(bad.clone())?,
        dialect: le16(b, 4).ok_or(bad.clone())?,
        server_guid,
        capabilities: le32(b, 24).ok_or(bad.clone())?,
        max_transact: le32(b, 28).ok_or(bad.clone())?,
        max_read: le32(b, 32).ok_or(bad.clone())?,
        max_write: le32(b, 36).ok_or(bad)?,
        system_time: le64(b, 40).unwrap_or(0),
        security_buffer: buffer(message, offset, len, WHAT)?.to_vec(),
    })
}

/// The SESSION_SETUP request body carrying `token`.
pub fn session_setup_request(security_mode: u8, token: &[u8]) -> Result<Vec<u8>, Error> {
    let len =
        u16::try_from(token.len()).map_err(|_| Error::Malformed("security token too long"))?;
    let mut out = Vec::with_capacity(24 + token.len());
    out.extend_from_slice(&25u16.to_le_bytes());
    out.push(0);
    out.push(security_mode);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&((HEADER + 24) as u16).to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(token);
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSetupResponse {
    pub flags: u16,
    pub token: Vec<u8>,
}

pub fn parse_session_setup(message: &[u8]) -> Result<SessionSetupResponse, Error> {
    const WHAT: &str = "SESSION_SETUP response";
    let b = body(message, 9, WHAT)?;
    let bad = Error::Malformed(WHAT);
    let offset = le16(b, 4).ok_or(bad.clone())? as usize;
    let len = le16(b, 6).ok_or(bad.clone())? as usize;
    Ok(SessionSetupResponse {
        flags: le16(b, 2).ok_or(bad)?,
        token: buffer(message, offset, len, WHAT)?.to_vec(),
    })
}

/// The TREE_CONNECT request body for a UNC path (UTF-16LE).
pub fn tree_connect_request(unc: &[u8]) -> Result<Vec<u8>, Error> {
    let len = u16::try_from(unc.len()).map_err(|_| Error::BadName)?;
    let mut out = Vec::with_capacity(8 + unc.len());
    out.extend_from_slice(&9u16.to_le_bytes());
    out.extend_from_slice(&[0; 2]);
    out.extend_from_slice(&((HEADER + 8) as u16).to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(unc);
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeConnectResponse {
    pub share_type: u8,
    pub share_flags: u32,
    pub capabilities: u32,
    pub maximal_access: u32,
}

pub fn parse_tree_connect(message: &[u8]) -> Result<TreeConnectResponse, Error> {
    const WHAT: &str = "TREE_CONNECT response";
    let b = body(message, 16, WHAT)?;
    let bad = Error::Malformed(WHAT);
    Ok(TreeConnectResponse {
        share_type: b[2],
        share_flags: le32(b, 4).ok_or(bad.clone())?,
        capabilities: le32(b, 8).ok_or(bad.clone())?,
        maximal_access: le32(b, 12).ok_or(bad)?,
    })
}
