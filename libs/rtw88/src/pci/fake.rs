//! Fakes for host tests and fuzzing: a register file behind [`Mmio`] with a
//! small model of the DBI and MDIO ports, and heap memory behind
//! [`DmaAlloc`] with made-up device addresses.
//!
//! The register file is dumb (a register reads back what was written, like
//! [`crate::fake::FakeHci`]) so a test states the chip behaviour it relies
//! on: it writes a ring's hardware index or an interrupt status with
//! [`FakeMmio::set32`] and reads the access log.

use core::cell::RefCell;
use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::collections::BTreeMap;
use std::vec::Vec;

use crate::error::{Error, Result};

use super::regs::*;
use super::{DmaAlloc, DmaBuf, Mmio};

/// One register access, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read { width: u8, addr: u32, val: u32 },
    Write { width: u8, addr: u32, val: u32 },
}

#[derive(Default)]
struct State {
    regs: Vec<u8>,
    log: Vec<Access>,
    delayed_us: u64,
    /// The PCIe core's space behind DBI.
    dbi: BTreeMap<u16, u8>,
    /// MDIO writes: `(page, addr, data)`.
    mdio: Vec<(u8, u8, u16)>,
}

/// The fake BAR.
pub struct FakeMmio {
    st: RefCell<State>,
    /// When set, DBI and MDIO never finish (their busy flags stay set).
    pub stuck: bool,
}

impl Default for FakeMmio {
    fn default() -> Self {
        FakeMmio {
            st: RefCell::new(State {
                regs: std::vec![0; 0x1_0000],
                ..State::default()
            }),
            stuck: false,
        }
    }
}

impl FakeMmio {
    fn load(&self, addr: u32, width: u8) -> u32 {
        let st = self.st.borrow();
        (0..u32::from(width)).fold(0, |v, i| {
            v | u32::from(st.regs[((addr + i) & 0xFFFF) as usize]) << (8 * i)
        })
    }

    fn store(&self, addr: u32, width: u8, val: u32) {
        let mut st = self.st.borrow_mut();
        for i in 0..u32::from(width) {
            st.regs[((addr + i) & 0xFFFF) as usize] = (val >> (8 * i)) as u8;
        }
    }

    /// Set a register without logging (the hardware's side).
    pub fn set32(&self, addr: u32, val: u32) {
        self.store(addr, 4, val);
    }

    /// Read a register without logging.
    pub fn peek32(&self, addr: u32) -> u32 {
        self.load(addr, 4)
    }

    pub fn peek16(&self, addr: u32) -> u16 {
        self.load(addr, 2) as u16
    }

    pub fn peek8(&self, addr: u32) -> u8 {
        self.load(addr, 1) as u8
    }

    pub fn log(&self) -> Vec<Access> {
        self.st.borrow().log.clone()
    }

    pub fn clear_log(&self) {
        self.st.borrow_mut().log.clear();
    }

    /// Every write, in order, as `(width, addr, val)`.
    pub fn writes(&self) -> Vec<(u8, u32, u32)> {
        self.st
            .borrow()
            .log
            .iter()
            .filter_map(|a| match *a {
                Access::Write { width, addr, val } => Some((width, addr, val)),
                _ => None,
            })
            .collect()
    }

    /// The last value written to `addr` with `width`.
    pub fn last_write(&self, width: u8, addr: u32) -> Option<u32> {
        self.writes()
            .into_iter()
            .rev()
            .find(|w| w.0 == width && w.1 == addr)
            .map(|w| w.2)
    }

    pub fn delayed_us(&self) -> u64 {
        self.st.borrow().delayed_us
    }

    pub fn dbi(&self, addr: u16) -> u8 {
        self.st.borrow().dbi.get(&addr).copied().unwrap_or(0)
    }

    pub fn set_dbi(&self, addr: u16, val: u8) {
        self.st.borrow_mut().dbi.insert(addr, val);
    }

    pub fn mdio(&self) -> Vec<(u8, u8, u16)> {
        self.st.borrow().mdio.clone()
    }

    /// The DBI and MDIO engines react to their "go" writes.
    fn after_write(&self, addr: u32, width: u8, val: u32) {
        if self.stuck {
            return;
        }
        if addr == REG_DBI_FLAG_V1 + 2 && width == 1 {
            let cmd = self.load(REG_DBI_FLAG_V1, 2) as u16;
            let dword = cmd & BITS_DBI_ADDR_MASK;
            let mut st = self.st.borrow_mut();
            if val == BIT_DBI_WFLAG >> 16 {
                for lane in 0..4u16 {
                    if cmd & (1 << (12 + lane)) != 0 {
                        let byte = st.regs[(REG_DBI_WDATA_V1 + u32::from(lane)) as usize];
                        st.dbi.insert(dword + lane, byte);
                    }
                }
            } else if val == BIT_DBI_RFLAG >> 16 {
                for lane in 0..4u16 {
                    let byte = st.dbi.get(&(dword + lane)).copied().unwrap_or(0);
                    st.regs[(REG_DBI_RDATA_V1 + u32::from(lane)) as usize] = byte;
                }
            }
            st.regs[(REG_DBI_FLAG_V1 + 2) as usize] = 0;
        }
        if addr == REG_PCIE_MIX_CFG && width == 4 && val & BIT_MDIO_WFLAG_V1 != 0 {
            let data = self.load(REG_MDIO_V1, 2) as u16;
            let reg = self.load(REG_PCIE_MIX_CFG, 1) as u8 & BITS_MDIO_ADDR_MASK;
            let page = self.load(REG_PCIE_MIX_CFG + 3, 1) as u8;
            self.st.borrow_mut().mdio.push((page, reg, data));
            self.store(REG_PCIE_MIX_CFG, 4, val & !BIT_MDIO_WFLAG_V1);
        }
    }

    fn read(&self, addr: u32, width: u8) -> u32 {
        let val = self.load(addr, width);
        self.st
            .borrow_mut()
            .log
            .push(Access::Read { width, addr, val });
        val
    }

    fn write(&self, addr: u32, width: u8, val: u32) {
        self.store(addr, width, val);
        self.st
            .borrow_mut()
            .log
            .push(Access::Write { width, addr, val });
        self.after_write(addr, width, val);
    }
}

impl Mmio for FakeMmio {
    fn read8(&self, offset: u32) -> u8 {
        self.read(offset, 1) as u8
    }
    fn read16(&self, offset: u32) -> u16 {
        self.read(offset, 2) as u16
    }
    fn read32(&self, offset: u32) -> u32 {
        self.read(offset, 4)
    }
    fn write8(&self, offset: u32, val: u8) {
        self.write(offset, 1, val.into())
    }
    fn write16(&self, offset: u32, val: u16) {
        self.write(offset, 2, val.into())
    }
    fn write32(&self, offset: u32, val: u32) {
        self.write(offset, 4, val)
    }
    fn delay_us(&self, us: u32) {
        self.st.borrow_mut().delayed_us += u64::from(us);
    }
}

/// Heap memory with device addresses counting up from `next_phys`.
pub struct HeapDma {
    blocks: Vec<DmaBuf>,
    pub next_phys: u64,
    /// Hand out blocks one byte off a page (to test the check).
    pub misalign: bool,
    /// Fail after this many blocks.
    pub limit: usize,
}

impl Default for HeapDma {
    fn default() -> Self {
        HeapDma {
            blocks: Vec::new(),
            next_phys: 0x1000_0000,
            misalign: false,
            limit: 16,
        }
    }
}

impl HeapDma {
    /// The CPU address of device address `phys`, if `len` bytes from there
    /// lie inside one block.
    pub fn virt(&self, phys: u64, len: usize) -> Option<*mut u8> {
        self.blocks.iter().find_map(|b| {
            let end = phys.checked_add(len as u64)?;
            (phys >= b.phys && end <= b.phys + b.len as u64)
                // SAFETY: `phys..phys + len` is inside the block, checked above.
                .then(|| unsafe { b.virt.add((phys - b.phys) as usize) })
        })
    }

    /// `len` bytes at device address `phys` (a copy).
    pub fn read(&self, phys: u64, len: usize) -> Vec<u8> {
        let p = self.virt(phys, len).expect("range in a block");
        // SAFETY: `virt` checked the whole range is inside a live heap block.
        unsafe { core::slice::from_raw_parts(p, len) }.to_vec()
    }

    /// Write `data` at device address `phys` (the device's side).
    pub fn write(&self, phys: u64, data: &[u8]) {
        let p = self.virt(phys, data.len()).expect("range in a block");
        // SAFETY: as in `read`; nothing else holds a reference to it.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len()) };
    }
}

// SAFETY: every block is a fresh zeroed heap allocation of `len` bytes,
// 4096 aligned (unless `misalign` asks otherwise, which `Pci` refuses),
// freed only when the allocator drops, after the `Pci` that owns it.
unsafe impl DmaAlloc for HeapDma {
    fn alloc(&mut self, len: usize) -> Result<DmaBuf> {
        if self.blocks.len() >= self.limit {
            return Err(Error::NoMemory);
        }
        let layout = Layout::from_size_align(len + 4096, 4096).map_err(|_| Error::NoMemory)?;
        // SAFETY: `layout` has a non-zero size.
        let base = unsafe { alloc_zeroed(layout) };
        if base.is_null() {
            return Err(Error::NoMemory);
        }
        let skew = usize::from(self.misalign);
        let buf = DmaBuf {
            // SAFETY: the allocation has 4096 spare bytes.
            virt: unsafe { base.add(skew) },
            phys: self.next_phys,
            len,
        };
        self.next_phys += (len as u64).div_ceil(4096) * 4096 + 4096;
        self.blocks.push(buf);
        Ok(buf)
    }
}

impl Drop for HeapDma {
    fn drop(&mut self) {
        let skew = usize::from(self.misalign);
        for b in &self.blocks {
            let layout = Layout::from_size_align(b.len + 4096, 4096).unwrap();
            // SAFETY: allocated in `alloc` with this layout at `virt - skew`.
            unsafe { dealloc(b.virt.sub(skew), layout) };
        }
    }
}
