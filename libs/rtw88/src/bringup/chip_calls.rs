//! TEMPORARY STAND-INS, replace at merge: the chip work package's
//! functions the MAC package calls, with the signatures it was asked to
//! provide (`crate::rtw8822c` and `crate::coex` module docs). They did not
//! exist in this worktree, so each is a no-op here and the crate builds and
//! its tests run. At merge, point each call site at the real function
//! (`chip_calls::x` becomes `crate::rtw8822c::x`, `crate::coex::x` or
//! `crate::phy::x` as noted) and delete this file.
//!
//! None of these may stay a no-op on hardware: without `mac_init`,
//! `phy_set_param` and `set_channel` the radio neither receives nor sends.

use crate::dev::{Bw, Dev};
use crate::efuse::Efuse;
use crate::error::Result;
use crate::hci::Hci;

/// contract: `crate::rtw8822c::read_efuse(&mut Efuse, log_map: &[u8]) -> Result<()>`
/// (upstream `rtw8822c_read_efuse`).
pub fn read_efuse(_efuse: &mut Efuse, _log_map: &[u8]) -> Result<()> {
    Ok(())
}

/// contract: `crate::rtw8822c::mac_init(dev) -> Result<()>` (upstream
/// `rtw8822c_mac_init`).
pub fn mac_init<H: Hci>(_dev: &mut Dev<H>) -> Result<()> {
    Ok(())
}

/// contract: `crate::rtw8822c::phy_set_param(dev) -> Result<()>` (upstream
/// `rtw8822c_phy_set_param`).
pub fn phy_set_param<H: Hci>(_dev: &mut Dev<H>) -> Result<()> {
    Ok(())
}

/// contract: `crate::rtw8822c::set_channel(dev, channel, bw, primary_chan_idx)`
/// (upstream `rtw8822c_set_channel`; it calls [`crate::mac::set_channel_mac`]).
pub fn set_channel<H: Hci>(_dev: &mut Dev<H>, _channel: u8, _bw: Bw, _primary_chan_idx: u8) {}

/// contract: `crate::phy::set_tx_power_level(dev, channel)` (upstream
/// `rtw_phy_set_tx_power_level`, ending in `rtw8822c::set_tx_power_index`).
pub fn set_tx_power_level<H: Hci>(_dev: &mut Dev<H>, _channel: u8) {}

/// contract: `crate::rtw8822c::phy_calibration(dev)` (upstream
/// `rtw8822c_phy_calibration`).
pub fn phy_calibration<H: Hci>(_dev: &mut Dev<H>) {}

/// contract: `crate::phy::setup_board_info(dev) -> Result<()>`, the PHY half
/// of upstream `rtw_chip_board_info_setup` (`rtw_phy_setup_phy_cond`,
/// `rtw_phy_init_tx_power`, the RFE's power-by-rate and limit tables).
pub fn board_info_setup<H: Hci>(_dev: &mut Dev<H>) -> Result<()> {
    Ok(())
}

/// contract: `crate::coex::power_on_setting(dev)` (upstream
/// `rtw_coex_power_on_setting`).
pub fn coex_power_on_setting<H: Hci>(_dev: &mut Dev<H>) {}

/// contract: `crate::coex::init_hw_config(dev, wifi_only)` (upstream
/// `rtw_coex_init_hw_config`).
pub fn coex_init_hw_config<H: Hci>(_dev: &mut Dev<H>, _wifi_only: bool) {}

/// contract: `crate::coex::power_off_setting(dev)` (upstream
/// `rtw_coex_power_off_setting`).
pub fn coex_power_off_setting<H: Hci>(_dev: &mut Dev<H>) {}

/// Upstream `rtw8822c_cfg_ldo25` (`chip->ops->cfg_ldo25`), small enough to
/// carry here for the efuse read; the chip package may own it at merge.
/// `REG_ANAPARLDO_POW_MAC` (0x29) bit 0 is `BIT_LDOE25_PON`.
pub fn cfg_ldo25<H: Hci>(dev: &mut Dev<H>, enable: bool) {
    const REG_ANAPARLDO_POW_MAC: u32 = 0x0029;
    const BIT_LDOE25_PON: u8 = 1 << 0;
    let v = dev.hci.read8(REG_ANAPARLDO_POW_MAC);
    let v = if enable { v | BIT_LDOE25_PON } else { v & !BIT_LDOE25_PON };
    dev.hci.write8(REG_ANAPARLDO_POW_MAC, v);
}
