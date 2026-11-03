//! Return parameters of the Command_Complete events this stack reads.

use crate::addr::BdAddr;
use crate::wire::Reader;
use crate::Error;

/// The status octet every return-parameter block starts with.
pub fn status(return_params: &[u8]) -> Result<u8, Error> {
    return_params.first().copied().ok_or(Error::Truncated)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalVersion {
    pub hci_version: u8,
    pub hci_revision: u16,
    pub lmp_version: u8,
    pub manufacturer: u16,
    pub lmp_subversion: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BufferSize {
    /// Largest ACL data payload the controller accepts in one packet.
    pub acl_len: u16,
    /// Packets the controller can buffer.
    pub acl_num: u16,
}

/// Read_Encryption_Key_Size: status, connection handle, key size in octets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncryptionKeySize {
    pub status: u8,
    pub handle: u16,
    pub key_size: u8,
}

pub fn parse_encryption_key_size(rp: &[u8]) -> Result<EncryptionKeySize, Error> {
    let mut r = Reader::new(rp);
    Ok(EncryptionKeySize {
        status: r.u8()?,
        handle: r.u16()?,
        key_size: r.u8()?,
    })
}

pub fn encode_encryption_key_size(v: EncryptionKeySize) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec![v.status];
    out.extend_from_slice(&v.handle.to_le_bytes());
    out.push(v.key_size);
    out
}

pub fn parse_bd_addr(rp: &[u8]) -> Result<BdAddr, Error> {
    let mut r = Reader::new(rp);
    r.u8()?;
    r.addr()
}

pub fn parse_local_version(rp: &[u8]) -> Result<LocalVersion, Error> {
    let mut r = Reader::new(rp);
    r.u8()?;
    Ok(LocalVersion {
        hci_version: r.u8()?,
        hci_revision: r.u16()?,
        lmp_version: r.u8()?,
        manufacturer: r.u16()?,
        lmp_subversion: r.u16()?,
    })
}

/// Read_Buffer_Size: status, ACL length, SCO length, ACL count, SCO count.
pub fn parse_buffer_size(rp: &[u8]) -> Result<BufferSize, Error> {
    let mut r = Reader::new(rp);
    r.u8()?;
    let acl_len = r.u16()?;
    r.u8()?;
    let acl_num = r.u16()?;
    Ok(BufferSize { acl_len, acl_num })
}

/// LE_Read_Buffer_Size: status, LE ACL length, LE ACL count. A zero length
/// means the controller shares the BR/EDR buffers.
pub fn parse_le_buffer_size(rp: &[u8]) -> Result<BufferSize, Error> {
    let mut r = Reader::new(rp);
    r.u8()?;
    Ok(BufferSize {
        acl_len: r.u16()?,
        acl_num: u16::from(r.u8()?),
    })
}

pub fn encode_bd_addr(addr: BdAddr) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec![0];
    out.extend_from_slice(&addr.0);
    out
}

pub fn encode_local_version(v: &LocalVersion) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec![0, v.hci_version];
    out.extend_from_slice(&v.hci_revision.to_le_bytes());
    out.push(v.lmp_version);
    out.extend_from_slice(&v.manufacturer.to_le_bytes());
    out.extend_from_slice(&v.lmp_subversion.to_le_bytes());
    out
}

pub fn encode_buffer_size(b: BufferSize) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec![0];
    out.extend_from_slice(&b.acl_len.to_le_bytes());
    out.push(0);
    out.extend_from_slice(&b.acl_num.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

pub fn encode_le_buffer_size(b: BufferSize) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec![0];
    out.extend_from_slice(&b.acl_len.to_le_bytes());
    out.push(b.acl_num.min(255) as u8);
    out
}
