//! The register accessor everything above the hardware goes through.
//!
//! The display engine's registers are 32-bit and 4-byte aligned in BAR 0
//! (the lower half of the 16 MiB BAR; the GGTT is the upper half). Code that
//! programs them takes `&mut impl Mmio`, so a host fake can check sequences.

/// 32-bit memory-mapped register access at a byte offset into BAR 0.
pub trait Mmio {
    /// Read the register at `offset`. Takes `&mut self` because some Gen9
    /// registers have read side effects (CRC and status reads latch).
    fn read32(&mut self, offset: u32) -> u32;
    /// Write the register at `offset`.
    fn write32(&mut self, offset: u32, value: u32);

    /// Read-modify-write: clear `mask` bits, then set `value & mask`.
    fn update32(&mut self, offset: u32, mask: u32, value: u32) -> u32 {
        let new = (self.read32(offset) & !mask) | (value & mask);
        self.write32(offset, new);
        new
    }
}

/// A register file in memory, for host tests: unwritten registers read 0 and
/// every access is kept in order in `log`.
#[cfg(any(test, feature = "fuzz"))]
#[derive(Default)]
pub struct FakeMmio {
    pub regs: std::collections::BTreeMap<u32, u32>,
    /// `(offset, value, is_write)` in access order.
    pub log: std::vec::Vec<(u32, u32, bool)>,
}

#[cfg(any(test, feature = "fuzz"))]
impl Mmio for FakeMmio {
    fn read32(&mut self, offset: u32) -> u32 {
        let value = self.regs.get(&offset).copied().unwrap_or(0);
        self.log.push((offset, value, false));
        value
    }

    fn write32(&mut self, offset: u32, value: u32) {
        self.log.push((offset, value, true));
        self.regs.insert(offset, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_changes_only_the_masked_bits() {
        let mut m = FakeMmio::default();
        m.regs.insert(0x46000, 0xF0F0_0001);
        let new = m.update32(0x46000, 0x0000_FF00, 0x1234_5678);
        assert_eq!(new, 0xF0F0_5601);
        assert_eq!(m.regs[&0x46000], 0xF0F0_5601);
        assert_eq!(m.log.len(), 2);
        assert!(!m.log[0].2 && m.log[1].2);
    }
}
