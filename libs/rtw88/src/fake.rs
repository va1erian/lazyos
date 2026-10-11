//! A fake bus for host tests: a 64 KiB byte-addressed register file that
//! remembers every access, plus the packets handed to each queue.
//!
//! It is deliberately dumb (a register reads back what was written) so a
//! test states the chip behaviour it relies on: `force` pins bits a poll
//! waits for, and the access log lets a test compare a sequence against
//! upstream's.

use std::vec::Vec;

use crate::error::Result;
use crate::hci::{Hci, TxQueue};

/// One register access, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read { width: u8, addr: u32, val: u32 },
    Write { width: u8, addr: u32, val: u32 },
    Delay { us: u32 },
}

/// The fake bus.
pub struct FakeHci {
    pub regs: Vec<u8>,
    pub log: Vec<Access>,
    /// Bits forced on reads: `(addr, mask, value)`; the read returns
    /// `(stored & !mask) | (value & mask)` for every byte covered.
    pub force: Vec<(u32, u32, u32)>,
    pub tx: Vec<(TxQueue, Vec<u8>)>,
    pub rsvd: Vec<Vec<u8>>,
    pub h2c: Vec<Vec<u8>>,
    pub kicks: usize,
    pub delayed_us: u64,
}

impl Default for FakeHci {
    fn default() -> Self {
        FakeHci {
            regs: std::vec![0; 0x1_0000],
            log: Vec::new(),
            force: Vec::new(),
            tx: Vec::new(),
            rsvd: Vec::new(),
            h2c: Vec::new(),
            kicks: 0,
            delayed_us: 0,
        }
    }
}

impl FakeHci {
    fn load(&self, addr: u32, width: u8) -> u32 {
        let mut v = 0u32;
        for i in 0..u32::from(width) {
            let a = (addr + i) as usize % self.regs.len();
            v |= u32::from(self.regs[a]) << (8 * i);
        }
        for &(faddr, mask, value) in &self.force {
            // Apply a forced 32-bit word to whichever bytes of it this read covers.
            for i in 0..u32::from(width) {
                let a = addr + i;
                if a >= faddr && a < faddr + 4 {
                    let shift = 8 * (a - faddr);
                    let m = (mask >> shift) & 0xFF;
                    let f = (value >> shift) & 0xFF;
                    let cur = (v >> (8 * i)) & 0xFF;
                    let new = (cur & !m) | (f & m);
                    v = (v & !(0xFF << (8 * i))) | (new << (8 * i));
                }
            }
        }
        v
    }

    fn store(&mut self, addr: u32, width: u8, val: u32) {
        for i in 0..u32::from(width) {
            let a = (addr + i) as usize % self.regs.len();
            self.regs[a] = (val >> (8 * i)) as u8;
        }
        self.log.push(Access::Write { width, addr, val });
    }

    fn read(&mut self, addr: u32, width: u8) -> u32 {
        let val = self.load(addr, width);
        self.log.push(Access::Read { width, addr, val });
        val
    }

    /// Every write, in order, as `(width, addr, val)`.
    pub fn writes(&self) -> Vec<(u8, u32, u32)> {
        self.log
            .iter()
            .filter_map(|a| match *a {
                Access::Write { width, addr, val } => Some((width, addr, val)),
                _ => None,
            })
            .collect()
    }

    /// The stored 32-bit value at `addr`, without logging a read.
    pub fn peek32(&self, addr: u32) -> u32 {
        self.load(addr, 4)
    }
}

impl Hci for FakeHci {
    fn read8(&mut self, addr: u32) -> u8 {
        self.read(addr, 1) as u8
    }
    fn read16(&mut self, addr: u32) -> u16 {
        self.read(addr, 2) as u16
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.read(addr, 4)
    }
    fn write8(&mut self, addr: u32, val: u8) {
        self.store(addr, 1, u32::from(val));
    }
    fn write16(&mut self, addr: u32, val: u16) {
        self.store(addr, 2, u32::from(val));
    }
    fn write32(&mut self, addr: u32, val: u32) {
        self.store(addr, 4, val);
    }
    fn delay_us(&mut self, us: u32) {
        self.delayed_us += u64::from(us);
        self.log.push(Access::Delay { us });
    }
    fn setup(&mut self) -> Result<()> {
        Ok(())
    }
    fn start(&mut self) -> Result<()> {
        Ok(())
    }
    fn stop(&mut self) {}
    fn interface_cfg(&mut self) {}
    fn link_ps(&mut self, _enter: bool) {}
    fn tx_write(&mut self, queue: TxQueue, pkt: &[u8]) -> Result<()> {
        self.tx.push((queue, pkt.to_vec()));
        Ok(())
    }
    fn tx_kick_off(&mut self) {
        self.kicks += 1;
    }
    fn write_data_rsvd_page(&mut self, pkt: &[u8]) -> Result<()> {
        self.rsvd.push(pkt.to_vec());
        Ok(())
    }
    fn write_data_h2c(&mut self, pkt: &[u8]) -> Result<()> {
        self.h2c.push(pkt.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hci::RegExt;

    #[test]
    fn mask_helpers_match_upstream() {
        let mut f = FakeHci::default();
        f.write32(0x100, 0xAABB_CCDD);
        f.write32_mask(0x100, 0x0000_FF00, 0x12);
        assert_eq!(f.peek32(0x100), 0xAABB_12DD);
        assert_eq!(f.read32_mask(0x100, 0x00FF_0000), 0xBB);
    }

    #[test]
    fn forced_bits_end_a_poll() {
        let mut f = FakeHci::default();
        assert!(f.poll8("t", 0x10, 0x1, 0x1, 10, 50).is_err());
        f.force.push((0x10, 0x1, 0x1));
        assert!(f.poll8("t", 0x10, 0x1, 0x1, 10, 50).is_ok());
        assert_eq!(f.read8(0x11), 0);
    }
}
