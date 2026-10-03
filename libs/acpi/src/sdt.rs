//! The RSDP, the common table header and the root tables (XSDT/RSDT).

use crate::{Error, PhysExt, PhysMem};

/// Size of the common System Description Table header.
pub const HEADER_LEN: u32 = 36;
/// Largest ordinary table accepted. Real FADTs, MADTs and HPET tables are a
/// few hundred bytes; an MADT for a 512-thread machine is about 8 KiB.
pub const MAX_TABLE_LEN: u32 = 64 * 1024;
/// Largest DSDT accepted (real ones reach a few hundred KiB).
pub const MAX_DSDT_LEN: u32 = 4 * 1024 * 1024;
/// Root entries examined at most; real machines list a few dozen tables.
pub const MAX_ROOT_ENTRIES: u32 = 256;
/// Largest ACPI 2.0+ RSDP length accepted (the structure is 36 bytes).
const MAX_RSDP_LEN: u32 = 1024;

/// A validated System Description Table header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sdt {
    pub phys: u64,
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
}

impl Sdt {
    /// Read and validate the table at `phys`: readable, `expected` signature
    /// (when given), length in `HEADER_LEN..=max_len` without wrapping the
    /// address space, and a zero byte sum over the whole length.
    pub fn load(
        mem: &dyn PhysMem,
        phys: u64,
        expected: Option<[u8; 4]>,
        max_len: u32,
    ) -> Result<Sdt, Error> {
        if phys == 0 {
            return Err(Error::NotFound);
        }
        let header = mem.bytes::<36>(phys)?;
        let signature = [header[0], header[1], header[2], header[3]];
        if expected.is_some_and(|sig| sig != signature) {
            return Err(Error::BadSignature);
        }
        let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        if !(HEADER_LEN..=max_len).contains(&length) {
            return Err(Error::BadLength);
        }
        phys.checked_add(u64::from(length))
            .ok_or(Error::BadLength)?;
        if checksum(mem, phys, length)? != 0 {
            return Err(Error::BadChecksum);
        }
        let mut oem_id = [0u8; 6];
        oem_id.copy_from_slice(&header[10..16]);
        let mut oem_table_id = [0u8; 8];
        oem_table_id.copy_from_slice(&header[16..24]);
        Ok(Sdt {
            phys,
            signature,
            length,
            revision: header[8],
            oem_id,
            oem_table_id,
        })
    }

    /// Physical address of byte `offset` of the table, if it lies inside it
    /// with `width` bytes to spare.
    pub fn field(&self, offset: u32, width: u32) -> Option<u64> {
        let end = offset.checked_add(width)?;
        (end <= self.length).then(|| self.phys + u64::from(offset))
    }
}

/// The byte sum of `len` bytes at `phys`, read in bounded chunks.
pub fn checksum(mem: &dyn PhysMem, phys: u64, len: u32) -> Result<u8, Error> {
    let mut sum = 0u8;
    let mut buf = [0u8; 256];
    let mut done = 0u32;
    while done < len {
        let chunk = (len - done).min(buf.len() as u32);
        let at = phys.checked_add(u64::from(done)).ok_or(Error::BadLength)?;
        if !mem.read(at, &mut buf[..chunk as usize]) {
            return Err(Error::Unreadable);
        }
        sum = buf[..chunk as usize]
            .iter()
            .fold(sum, |acc, b| acc.wrapping_add(*b));
        done += chunk;
    }
    Ok(sum)
}

/// A validated Root System Description Pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rsdp {
    pub phys: u64,
    /// 0 for ACPI 1.0, 2 for ACPI 2.0+ (the UEFI "XSDP").
    pub revision: u8,
    pub oem_id: [u8; 6],
    /// RSDT address (0 when absent).
    pub rsdt: u32,
    /// XSDT address, only when the revision is 2+ and the extended checksum
    /// and length are valid.
    pub xsdt: Option<u64>,
}

impl Rsdp {
    /// Read and validate the RSDP at `phys`. The 20-byte ACPI 1.0 part must
    /// checksum; the XSDT pointer is used only when the extended part does too.
    pub fn read(mem: &dyn PhysMem, phys: u64) -> Result<Rsdp, Error> {
        if phys == 0 {
            return Err(Error::NotFound);
        }
        let head = mem.bytes::<20>(phys)?;
        if &head[..8] != b"RSD PTR " {
            return Err(Error::BadSignature);
        }
        if head.iter().fold(0u8, |acc, b| acc.wrapping_add(*b)) != 0 {
            return Err(Error::BadChecksum);
        }
        let mut oem_id = [0u8; 6];
        oem_id.copy_from_slice(&head[9..15]);
        let revision = head[15];
        let rsdt = u32::from_le_bytes([head[16], head[17], head[18], head[19]]);
        let xsdt = if revision >= 2 {
            extended(mem, phys).ok().filter(|&x| x != 0)
        } else {
            None
        };
        if rsdt == 0 && xsdt.is_none() {
            return Err(Error::Malformed);
        }
        Ok(Rsdp {
            phys,
            revision,
            oem_id,
            rsdt,
            xsdt,
        })
    }
}

/// The XSDT address of an ACPI 2.0+ RSDP, after checking its length and
/// extended checksum.
fn extended(mem: &dyn PhysMem, phys: u64) -> Result<u64, Error> {
    let len = mem.u32_at(phys + 20)?;
    if !(36..=MAX_RSDP_LEN).contains(&len) {
        return Err(Error::BadLength);
    }
    if checksum(mem, phys, len)? != 0 {
        return Err(Error::BadChecksum);
    }
    mem.u64_at(phys + 24)
}

/// A validated root table and the width of its entries.
#[derive(Clone, Copy, Debug)]
pub struct Root {
    pub table: Sdt,
    /// 8 for the XSDT, 4 for the RSDT.
    pub width: u32,
    /// Entries to examine (capped at [`MAX_ROOT_ENTRIES`]).
    pub entries: u32,
}

impl Root {
    /// The physical address in entry `index`.
    pub fn entry(&self, mem: &dyn PhysMem, index: u32) -> Result<u64, Error> {
        let addr = self
            .table
            .field(HEADER_LEN + index * self.width, self.width)
            .ok_or(Error::BadLength)?;
        let value = if self.width == 8 {
            mem.u64_at(addr)?
        } else {
            u64::from(mem.u32_at(addr)?)
        };
        if value == 0 {
            Err(Error::NotFound)
        } else {
            Ok(value)
        }
    }
}

/// The XSDT when the RSDP names a valid one, else the RSDT.
pub fn root(mem: &dyn PhysMem, rsdp: &Rsdp) -> Result<Root, Error> {
    let xsdt = rsdp
        .xsdt
        .map(|addr| Sdt::load(mem, addr, Some(*b"XSDT"), MAX_TABLE_LEN));
    let (table, width) = match xsdt {
        Some(Ok(table)) => (table, 8),
        // An invalid XSDT falls back to the RSDT; only when there is none
        // does the XSDT's own error explain the failure.
        Some(Err(error)) if rsdp.rsdt == 0 => return Err(error),
        _ => (
            Sdt::load(mem, u64::from(rsdp.rsdt), Some(*b"RSDT"), MAX_TABLE_LEN)?,
            4,
        ),
    };
    let entries = ((table.length - HEADER_LEN) / width).min(MAX_ROOT_ENTRIES);
    Ok(Root {
        table,
        width,
        entries,
    })
}
