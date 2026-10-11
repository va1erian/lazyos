//! The LTE coexistence block's indirect registers, where the grant
//! controls live (upstream `util.c`: `ltecoex_read_reg`,
//! `ltecoex_reg_write`; `coex.c`: `rtw_coex_read_indirect_reg`,
//! `rtw_coex_write_indirect_reg`; the 8822C's `rtw8822c_ltecoex_addr`).

use crate::dev::Dev;
use crate::hci::Hci;
use crate::rtw8822c::regs::{LTECOEX_ACCESS_CTRL, LTECOEX_READY, LTECOEX_READ_DATA, LTECOEX_WRITE_DATA};
use crate::rtw8822c::util::check_hw_ready;

/// Upstream `rtw_coex_read_indirect_reg`: 0 if the block is not ready
/// (upstream logs "failed to read indirect register").
pub fn read_indirect_reg<H: Hci>(dev: &mut Dev<H>, addr: u16) -> u32 {
    let h = &mut dev.hci;
    if !check_hw_ready(h, LTECOEX_ACCESS_CTRL, LTECOEX_READY, 1) {
        return 0;
    }
    h.write32(LTECOEX_ACCESS_CTRL, 0x800f_0000 | u32::from(addr));
    h.read32(LTECOEX_READ_DATA)
}

/// Upstream `rtw_coex_write_indirect_reg`: read-modify-write of the field
/// under `mask`. A not-ready block drops the write (upstream logs it).
pub fn write_indirect_reg<H: Hci>(dev: &mut Dev<H>, addr: u16, mask: u32, val: u32) {
    let shift = mask.trailing_zeros();
    let tmp = read_indirect_reg(dev, addr);
    let tmp = (tmp & !mask) | (val.wrapping_shl(shift) & mask);
    let h = &mut dev.hci;
    if !check_hw_ready(h, LTECOEX_ACCESS_CTRL, LTECOEX_READY, 1) {
        return;
    }
    h.write32(LTECOEX_WRITE_DATA, tmp);
    h.write32(LTECOEX_ACCESS_CTRL, 0xc00f_0000 | u32::from(addr));
}
