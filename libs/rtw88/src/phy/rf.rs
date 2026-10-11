//! RF register access (upstream `phy.c`: `rtw_phy_read_rf`,
//! `rtw_phy_write_rf_reg`, `rtw_phy_write_rf_reg_sipi`,
//! `rtw_phy_write_rf_reg_mix`; the 8822C's `read_rf`/`write_rf` ops are the
//! first and the last).
//!
//! The RF chip's registers are 20 bits wide and 8-bit addressed. Reads go
//! through the direct window at `rf_base_addr[path] + addr * 4`. Writes go
//! through the same window, except register 0 (the RF mode register), which
//! upstream writes over the serial interface (SIPI, `rf_sipi_addr[path]`):
//! a direct write there races the hardware's own mode switching.

use crate::dev::Dev;
use crate::hci::{Hci, RegExt};
use crate::rtw8822c::spec::{RF_BASE_ADDR, RF_SIPI_ADDR};

/// Upstream `RFREG_MASK`: every bit of an RF register.
pub const RFREG_MASK: u32 = 0xfffff;
/// Upstream `INV_RF_DATA`: what a read of a path the chip lacks returns.
pub const INV_RF_DATA: u32 = 0xffff_ffff;

fn direct_addr(path: usize, addr: u32) -> u32 {
    RF_BASE_ADDR[path] + ((addr & 0xff) << 2)
}

fn path_ok<H: Hci>(dev: &Dev<H>, path: usize) -> bool {
    path < usize::from(dev.hal.rf_phy_num) && path < RF_BASE_ADDR.len()
}

/// Upstream `rtw_phy_read_rf`: the field under `mask` of RF register `addr`
/// on `path`, or [`INV_RF_DATA`] for a path the chip does not have.
pub fn read_rf<H: Hci>(dev: &mut Dev<H>, path: usize, addr: u32, mask: u32) -> u32 {
    if !path_ok(dev, path) {
        return INV_RF_DATA;
    }
    dev.hci.read32_mask(direct_addr(path, addr), mask & RFREG_MASK)
}

/// Upstream `rtw_phy_write_rf_reg`: a direct write of the field under
/// `mask`, then 1 us for the RF chip to latch it.
pub fn write_rf_reg<H: Hci>(dev: &mut Dev<H>, path: usize, addr: u32, mask: u32, data: u32) -> bool {
    if !path_ok(dev, path) {
        return false;
    }
    dev.hci.write32_mask(direct_addr(path, addr), mask & RFREG_MASK, data);
    dev.hci.delay_us(1);
    true
}

/// Upstream `rtw_phy_write_rf_reg_sipi`: a partial write reads the whole
/// register first (through `read_rf`), then one 28-bit word `addr << 20 |
/// data` goes to the path's SIPI register and the chip gets 13 us.
pub fn write_rf_reg_sipi<H: Hci>(dev: &mut Dev<H>, path: usize, addr: u32, mask: u32, data: u32) -> bool {
    if !path_ok(dev, path) {
        return false;
    }
    let addr = addr & 0xff;
    let mask = mask & RFREG_MASK;
    let mut data = data;
    if mask != RFREG_MASK {
        let old = read_rf(dev, path, addr, RFREG_MASK);
        if old == INV_RF_DATA {
            return false;
        }
        // As upstream: the shifted value is not clipped to the mask.
        let shift = mask.trailing_zeros();
        data = (old & !mask) | (data << shift);
    }
    let data_and_addr = ((addr << 20) | (data & 0x000f_ffff)) & 0x0fff_ffff;
    dev.hci.write32(RF_SIPI_ADDR[path], data_and_addr);
    dev.hci.delay_us(13);
    true
}

/// Upstream `rtw_phy_write_rf_reg_mix`, the 8822C's `write_rf`: SIPI for
/// register 0, the direct window for everything else.
pub fn write_rf<H: Hci>(dev: &mut Dev<H>, path: usize, addr: u32, mask: u32, data: u32) -> bool {
    if addr != 0x00 {
        write_rf_reg(dev, path, addr, mask, data)
    } else {
        write_rf_reg_sipi(dev, path, addr, mask, data)
    }
}
