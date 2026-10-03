//! The Generic Address Structure (ACPI 6.5, section 5.2.3.2).

use crate::{Error, PhysExt, PhysMem};

/// Address space of a [`Gas`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressSpace {
    Memory,
    Io,
    PciConfig,
    /// Any other space (embedded controller, SMBus, functional fixed
    /// hardware, ...): recorded, never accessed by the kernel.
    Other(u8),
}

impl AddressSpace {
    fn from_id(id: u8) -> AddressSpace {
        match id {
            0 => AddressSpace::Memory,
            1 => AddressSpace::Io,
            2 => AddressSpace::PciConfig,
            other => AddressSpace::Other(other),
        }
    }
}

/// A register location as firmware describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gas {
    pub space: AddressSpace,
    pub bit_width: u8,
    pub bit_offset: u8,
    /// 0 undefined (legacy), 1 byte, 2 word, 3 dword, 4 qword.
    pub access_size: u8,
    pub address: u64,
}

impl Gas {
    /// Size of the structure in a table.
    pub const LEN: u32 = 12;

    /// Decode the 12 bytes at `addr`. `None` for the all-zero "not present"
    /// encoding.
    pub fn read(mem: &dyn PhysMem, addr: u64) -> Result<Option<Gas>, Error> {
        let raw = mem.bytes::<12>(addr)?;
        Ok(Gas::decode(&raw))
    }

    /// Decode raw bytes; `None` when the address is zero.
    pub fn decode(raw: &[u8; 12]) -> Option<Gas> {
        let mut address = [0u8; 8];
        address.copy_from_slice(&raw[4..12]);
        let address = u64::from_le_bytes(address);
        (address != 0).then_some(Gas {
            space: AddressSpace::from_id(raw[0]),
            bit_width: raw[1],
            bit_offset: raw[2],
            access_size: raw[3],
            address,
        })
    }

    /// A legacy FADT block: an I/O port `bytes` wide (`None` when the port or
    /// the width is zero).
    pub fn io(port: u32, bytes: u8) -> Option<Gas> {
        (port != 0 && bytes != 0).then_some(Gas {
            space: AddressSpace::Io,
            bit_width: bytes.saturating_mul(8),
            bit_offset: 0,
            access_size: 0,
            address: u64::from(port),
        })
    }

    /// The I/O port, when this is an I/O register whose `bytes` bytes all
    /// fit in the 16-bit port space.
    pub fn port(&self, bytes: u16) -> Option<u16> {
        if self.space != AddressSpace::Io {
            return None;
        }
        let last = self.address.checked_add(u64::from(bytes.max(1)) - 1)?;
        (last <= 0xFFFF).then_some(self.address as u16)
    }
}
