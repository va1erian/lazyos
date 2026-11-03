//! The on-disk layout.
//!
//! ```text
//! "LZBT"  version:u16  count:u16  record*count  crc32:u32
//! record: length:u16 body            (length counts the body)
//! body:   addr[6] kind:u8 flags:u8 key_size:u8 link_key_type:u8 cod[3]
//!         input:u8 hid_flags:u8 link_key[16] ltk[16] ediv:u16 rand:u64
//!         irk[16] name_len:u8 name descriptor_len:u16 descriptor
//!         report_map:u16 protocol_mode:u16 n:u8 (value:u16 cccd:u16 id:u8 type:u8)*n
//! ```
//!
//! All integers are little-endian. `flags`: bit 0 authenticated, 1 Secure
//! Connections, 2 has a link key, 3 has an LTK, 4 has an IRK; other bits must
//! be zero. Keys that are absent are stored as zeros so every body has a fixed
//! prefix. The CRC-32 (IEEE) covers everything before it.
//!
//! Decoding is strict and canonical: a record must be consumed exactly, unknown
//! flag bits and trailing bytes are refused, and a file that decodes encodes
//! back to the same bytes.

use alloc::string::String;
use alloc::vec::Vec;

use bthci::BdAddr;

use crate::bond::{
    AddrKind, Bond, Gatt, InputClass, LinkKey, Ltk, ReportHandle, MAX_DESCRIPTOR, MAX_NAME,
    MAX_REPORT_HANDLES,
};
use crate::crc::crc32;
use crate::{BondStore, Error, MAGIC, MAX_BONDS, VERSION};

const F_AUTH: u8 = 1;
const F_SC: u8 = 2;
const F_LINK_KEY: u8 = 4;
const F_LTK: u8 = 8;
const F_IRK: u8 = 16;
const FLAGS_ALL: u8 = 31;

/// Largest body of one record.
pub const MAX_RECORD: usize =
    6 + 4 + 3 + 2 + 16 * 3 + 10 + 1 + MAX_NAME + 2 + MAX_DESCRIPTOR + 5 + MAX_REPORT_HANDLES * 6;
/// Largest possible file; the reader refuses anything above it before parsing.
pub const MAX_FILE: usize = 8 + MAX_BONDS * (2 + MAX_RECORD) + 4;

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.0.len() < n {
            return Err(Error::Truncated);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }
}

pub(crate) fn encode(store: &BondStore) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(store.bonds.len() as u16).to_le_bytes());
    for bond in &store.bonds {
        let body = encode_body(bond)?;
        out.extend_from_slice(&(body.len() as u16).to_le_bytes());
        out.extend_from_slice(&body);
    }
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

fn encode_body(b: &Bond) -> Result<Vec<u8>, Error> {
    b.validate()?;
    let mut flags = 0;
    flags |= if b.authenticated { F_AUTH } else { 0 };
    flags |= if b.secure_connections { F_SC } else { 0 };
    flags |= if b.link_key.is_some() { F_LINK_KEY } else { 0 };
    flags |= if b.ltk.is_some() { F_LTK } else { 0 };
    flags |= if b.irk.is_some() { F_IRK } else { 0 };
    let mut o = Vec::new();
    o.extend_from_slice(&b.addr.0);
    o.extend_from_slice(&[
        b.kind.code(),
        flags,
        b.key_size,
        b.link_key.as_ref().map_or(0, |k| k.key_type),
    ]);
    o.extend_from_slice(&b.class_of_device.to_le_bytes()[..3]);
    o.extend_from_slice(&[b.input.code(), b.hid_flags]);
    o.extend_from_slice(&b.link_key.as_ref().map_or([0; 16], |k| k.key));
    o.extend_from_slice(&b.ltk.as_ref().map_or([0; 16], |k| k.key));
    o.extend_from_slice(&b.ltk.as_ref().map_or(0, |k| k.ediv).to_le_bytes());
    o.extend_from_slice(&b.ltk.as_ref().map_or(0, |k| k.rand).to_le_bytes());
    o.extend_from_slice(&b.irk.unwrap_or([0; 16]));
    o.push(b.name.len() as u8);
    o.extend_from_slice(b.name.as_bytes());
    o.extend_from_slice(&(b.report_descriptor.len() as u16).to_le_bytes());
    o.extend_from_slice(&b.report_descriptor);
    o.extend_from_slice(&b.gatt.report_map.to_le_bytes());
    o.extend_from_slice(&b.gatt.protocol_mode.to_le_bytes());
    o.push(b.gatt.reports.len() as u8);
    for r in &b.gatt.reports {
        o.extend_from_slice(&r.value.to_le_bytes());
        o.extend_from_slice(&r.cccd.to_le_bytes());
        o.extend_from_slice(&[r.report_id, r.report_type]);
    }
    Ok(o)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<BondStore, Error> {
    if bytes.len() > MAX_FILE {
        return Err(Error::TooLarge);
    }
    if bytes.len() < 12 {
        return Err(Error::Truncated);
    }
    let (body, tail) = bytes.split_at(bytes.len() - 4);
    let stored = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    if crc32(body) != stored {
        return Err(Error::Checksum);
    }
    let mut r = Reader(body);
    if r.array::<4>()? != MAGIC {
        return Err(Error::Magic);
    }
    let version = r.u16()?;
    if version != VERSION {
        return Err(Error::Version(version));
    }
    let count = usize::from(r.u16()?);
    if count > MAX_BONDS {
        return Err(Error::TooLarge);
    }
    let mut store = BondStore::new();
    for _ in 0..count {
        let len = usize::from(r.u16()?);
        if len > MAX_RECORD {
            return Err(Error::TooLarge);
        }
        let bond = decode_body(r.take(len)?)?;
        store.insert_unique(bond)?;
    }
    if !r.0.is_empty() {
        return Err(Error::Trailing);
    }
    Ok(store)
}

fn decode_body(body: &[u8]) -> Result<Bond, Error> {
    let mut r = Reader(body);
    let addr = BdAddr(r.array()?);
    let kind = AddrKind::from_code(r.u8()?)?;
    let flags = r.u8()?;
    if flags & !FLAGS_ALL != 0 {
        return Err(Error::Invalid("flags"));
    }
    let key_size = r.u8()?;
    let key_type = r.u8()?;
    let cod = r.array::<3>()?;
    let input = InputClass::from_code(r.u8()?)?;
    let hid_flags = r.u8()?;
    let link_key = r.array::<16>()?;
    let ltk = r.array::<16>()?;
    let ediv = r.u16()?;
    let rand = u64::from_le_bytes(r.array()?);
    let irk = r.array::<16>()?;
    let name_len = usize::from(r.u8()?);
    if name_len > MAX_NAME {
        return Err(Error::TooLarge);
    }
    let name = String::from_utf8(r.take(name_len)?.to_vec()).map_err(|_| Error::Invalid("name"))?;
    let desc_len = usize::from(r.u16()?);
    if desc_len > MAX_DESCRIPTOR {
        return Err(Error::TooLarge);
    }
    let report_descriptor = r.take(desc_len)?.to_vec();
    let report_map = r.u16()?;
    let protocol_mode = r.u16()?;
    let n = usize::from(r.u8()?);
    if n > MAX_REPORT_HANDLES {
        return Err(Error::TooLarge);
    }
    let mut reports = Vec::with_capacity(n);
    for _ in 0..n {
        reports.push(ReportHandle {
            value: r.u16()?,
            cccd: r.u16()?,
            report_id: r.u8()?,
            report_type: r.u8()?,
        });
    }
    if !r.0.is_empty() {
        return Err(Error::Trailing);
    }
    let bond = Bond {
        addr,
        kind,
        authenticated: flags & F_AUTH != 0,
        secure_connections: flags & F_SC != 0,
        key_size,
        link_key: (flags & F_LINK_KEY != 0).then_some(LinkKey {
            key: link_key,
            key_type,
        }),
        ltk: (flags & F_LTK != 0).then_some(Ltk {
            key: ltk,
            ediv,
            rand,
        }),
        irk: (flags & F_IRK != 0).then_some(irk),
        class_of_device: u32::from(cod[0]) | u32::from(cod[1]) << 8 | u32::from(cod[2]) << 16,
        input,
        hid_flags,
        name,
        report_descriptor,
        gatt: Gatt {
            report_map,
            protocol_mode,
            reports,
        },
    };
    bond.validate()?;
    canonical_zero_check(&bond, key_type, &link_key, &ltk, ediv, rand, &irk)?;
    Ok(bond)
}

/// Absent keys must be stored as zeros, or `encode(decode(x)) != x`.
fn canonical_zero_check(
    b: &Bond,
    key_type: u8,
    lk: &[u8; 16],
    ltk: &[u8; 16],
    ediv: u16,
    rand: u64,
    irk: &[u8; 16],
) -> Result<(), Error> {
    let zero = |v: &[u8; 16]| v.iter().all(|&x| x == 0);
    let ok = (b.link_key.is_some() || (key_type == 0 && zero(lk)))
        && (b.ltk.is_some() || (zero(ltk) && ediv == 0 && rand == 0))
        && (b.irk.is_some() || zero(irk));
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid("stale key bytes"))
    }
}
