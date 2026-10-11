//! The LTE coexistence block's indirect registers, where the grant
//! controls live (`coex.c`: `rtw_coex_read_indirect_reg`,
//! `rtw_coex_write_indirect_reg`, over `crate::mac`'s `ltecoex_read_reg` /
//! `ltecoex_reg_write`).

use crate::dev::Dev;
use crate::hci::Hci;
use crate::mac::{ltecoex_read_reg, ltecoex_reg_write};

/// Upstream `rtw_coex_read_indirect_reg`: 0 if the block is not ready
/// (upstream logs "failed to read indirect register").
pub fn read_indirect_reg<H: Hci>(dev: &mut Dev<H>, addr: u16) -> u32 {
    ltecoex_read_reg(&mut dev.hci, addr).unwrap_or(0)
}

/// Upstream `rtw_coex_write_indirect_reg`: read-modify-write of the field
/// under `mask`. A not-ready block drops the write (upstream logs it).
pub fn write_indirect_reg<H: Hci>(dev: &mut Dev<H>, addr: u16, mask: u32, val: u32) {
    let shift = mask.trailing_zeros();
    let tmp = read_indirect_reg(dev, addr);
    let tmp = (tmp & !mask) | (val.wrapping_shl(shift) & mask);
    let _ = ltecoex_reg_write(&mut dev.hci, addr, tmp);
}
