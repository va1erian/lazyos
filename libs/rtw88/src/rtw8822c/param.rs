//! BB/RF bring-up (upstream `rtw8822c_phy_set_param` and what it calls:
//! `rtw8822c_header_file_init`, `rtw_phy_load_tables`, the crystal cap,
//! `rtw8822c_config_trx_mode`, `rtw_phy_init`, `rtw8822c_rf_init`,
//! `rtw8822c_pwrtrack_init`, `rtw_bf_phy_init`), plus the small chip ops
//! (`rtw8822c_cfg_ldo25`, `rtw8822c_cfo_init`, crystal cap setters,
//! `rtw8822c_adaptivity_init`).

use crate::dev::{Dev, RF_PATH_MAX};
use crate::error::Result;
use crate::hci::{Hci, RegExt};
use crate::phy::{self, rate::DESC_RATE6M};
use crate::rtw8822c::efuse::XCAP_MASK;
use crate::rtw8822c::regs::*;
use crate::rtw8822c::trx::config_trx_mode;
use crate::rtw8822c::util::set8;
use crate::rtw8822c::{dack, trim};

/// Upstream `RTW8822C_EDCCA_MAX`.
const EDCCA_MAX: u32 = 0x7f;
/// The EDCCA threshold register's offset (`rtw8822c_edcca_th[].offset`).
const EDCCA_OFFSET: u32 = 0x80;

/// Upstream `rtw8822c_phy_set_param`. Runs after the MAC is powered and
/// the firmware is up (upstream `rtw_mac_init` order is the core's).
pub fn phy_set_param<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    // Power on the BB and RF domains.
    set8(&mut dev.hci, REG_SYS_FUNC_EN, BIT_FEN_BB_GLB_RST | BIT_FEN_BB_RSTB);
    set8(&mut dev.hci, REG_RF_CTRL, BIT_RF_EN | BIT_RF_RSTB | BIT_RF_SDM_RSTB);
    dev.hci.set32(REG_WLRF1, BIT_WLRF1_BBRF_EN);
    // No DPD on the low rates.
    dev.hci.write32_mask(REG_DIS_DPD, DIS_DPD_MASK, DIS_DPD_RATEALL);

    header_file_init(&mut dev.hci, true);
    phy::load_tables(dev);

    let cap = u32::from(dev.efuse.crystal_cap & XCAP_MASK);
    dev.hci.write32_mask(REG_ANAPAR_XTAL_0, 0xfffc00, cap | (cap << 7));

    header_file_init(&mut dev.hci, false);

    let (tx, rx) = (dev.hal.antenna_tx as u8, dev.hal.antenna_rx as u8);
    config_trx_mode(dev, tx, rx, false);
    phy_init(dev);

    // The CCK gain-index window the PHY status page 0 correction uses.
    let h = &mut dev.hci;
    let u_msb = h.read32_mask(0x1a98, 0xc000) as u8;
    let u_lsb = h.read32_mask(0x1aa8, 0xf0000) as u8;
    let l_msb = h.read32_mask(0x1a98, 0xc0) as u8;
    let l_lsb = h.read32_mask(0x1a70, 0x0f00_0000) as u8;
    dev.dm.cck_gi_u_bnd = (u_msb << 4) | u_lsb;
    dev.dm.cck_gi_l_bnd = (l_msb << 4) | l_lsb;

    rf_init(dev);
    pwrtrack_init(dev);
    bf_phy_init(&mut dev.hci);
    let ctx = crate::rtw8822c::phy_status_ctx(dev);
    dev.hci.set_phy_status_ctx(ctx);
    Ok(())
}

/// Upstream `rtw8822c_header_file_init`: 3-wire TX/RX and PI on for both
/// paths; CCK/OFDM blocks off while the tables load (`pre`), on after.
pub fn header_file_init<H: Hci>(h: &mut H, pre: bool) {
    h.set32(REG_3WIRE, BIT_3WIRE_TX_EN | BIT_3WIRE_RX_EN);
    h.set32(REG_3WIRE, BIT_3WIRE_PI_ON);
    h.set32(REG_3WIRE2, BIT_3WIRE_TX_EN | BIT_3WIRE_RX_EN);
    h.set32(REG_3WIRE2, BIT_3WIRE_PI_ON);
    if pre {
        h.clr32(REG_ENCCK, BIT_CCK_OFDM_BLK_EN);
    } else {
        h.set32(REG_ENCCK, BIT_CCK_OFDM_BLK_EN);
    }
}

/// Upstream `rtw8822c_rf_init`: DAC calibration, the X2 synthesiser check,
/// then the factory trims from the physical efuse.
pub fn rf_init<H: Hci>(dev: &mut Dev<H>) {
    dack::rf_dac_cal(dev);
    trim::rf_x2_check(dev);
    trim::thermal_trim(dev);
    trim::power_trim(dev);
    trim::pa_bias(dev);
}

/// Upstream `rtw_phy_init` with the 8822C's hooks: the DIG history starts
/// from the current IGI, IQK is not done, EDCCA in normal mode
/// (`rtw8822c_adaptivity_init`; no country, so no ETSI adaptivity), CFO
/// tracking from the efuse crystal cap.
pub fn phy_init<H: Hci>(dev: &mut Dev<H>) {
    dev.dm.igi_history0 = dev.hci.read32_mask(REG_RXIGI, 0x7f) as u8;
    dev.dm.iqk_done = false;
    adaptivity_init(&mut dev.hci);
    cfo_init(dev);
}

/// Upstream `rtw8822c_adaptivity_init` (with `rtw_phy_set_edcca_th`).
pub fn adaptivity_init<H: Hci>(h: &mut H) {
    h.write32_mask(REG_EDCCA_TH, MASKBYTE2, EDCCA_MAX + EDCCA_OFFSET);
    h.write32_mask(REG_EDCCA_TH, MASKBYTE3, EDCCA_MAX + EDCCA_OFFSET);
    h.clr32(REG_TX_PTCL_CTRL, BIT_DIS_EDCCA);
    h.set32(REG_RD_CTRL, BIT_EDCCA_MSK_CNTDOWN_EN);
    h.clr32(REG_EDCCA_DECISION, BIT_EDCCA_OPTION);
}

/// Upstream `rtw8822c_pwrtrack_init`.
pub fn pwrtrack_init<H: Hci>(dev: &mut Dev<H>) {
    let dm = &mut dev.dm;
    for path in 0..RF_PATH_MAX {
        dm.delta_power_index[path] = 0;
        dm.thermal_avg[path] = 0xff;
    }
    dm.pwr_trk_triggered = false;
    dm.thermal_meter_k = dev.efuse.thermal_meter_k;
    dm.thermal_meter_lck = dev.efuse.thermal_meter_k;
}

/// Upstream `rtw_bf_phy_init`: MU-MIMO off until sounding, retry limit 10,
/// NDPA at 6M, CSI at 6M.
pub fn bf_phy_init<H: Hci>(h: &mut H) {
    let mut v = h.read32(REG_MU_TX_CTL);
    v |= BIT_MU_P1_WAIT_STATE_EN;
    v &= !BIT_MASK_R_MU_RL;
    v |= (0xa << BIT_SHIFT_R_MU_RL) & BIT_MASK_R_MU_RL;
    v &= !BIT_EN_MU_MIMO;
    v &= !BIT_MASK_R_MU_TABLE_VALID;
    h.write32(REG_MU_TX_CTL, v);
    h.write8(REG_WMAC_MU_BF_OPTION, ((3 << BIT_SHIFT_WMAC_TXMU_ACKPOLICY) | BIT_WMAC_TXMU_ACKPOLICY_EN) as u8);
    h.write16(REG_WMAC_MU_BF_CTL, 0);
    h.set32(REG_TXBF_CTRL, BIT_USE_NDPA_PARAMETER);
    h.write8(REG_NDPA_OPT_CTRL, 0x10);
    h.write32_mask(REG_BBPSF_CTRL, BIT_MASK_CSI_RATE, u32::from(DESC_RATE6M));
}

/// Upstream `rtw8822c_cfg_ldo25`: the 2.5 V LDO (efuse programming power).
pub fn cfg_ldo25<H: Hci>(dev: &mut Dev<H>, enable: bool) {
    let v = dev.hci.read8(REG_ANAPARLDO_POW_MAC);
    let bit = BIT_LDOE25_PON as u8;
    dev.hci.write8(REG_ANAPARLDO_POW_MAC, if enable { v | bit } else { v & !bit });
}

/// Upstream `rtw8822c_set_crystal_cap_reg`.
pub fn set_crystal_cap_reg<H: Hci>(dev: &mut Dev<H>, cap: u8) {
    dev.dm.crystal_cap = cap;
    let v = u32::from(cap);
    dev.hci.write32_mask(REG_ANAPAR_XTAL_0, BIT_XCAP_0, v | (v << 7));
}

/// Upstream `rtw8822c_set_crystal_cap`: only on a change.
pub fn set_crystal_cap<H: Hci>(dev: &mut Dev<H>, cap: u8) {
    if dev.dm.crystal_cap != cap {
        set_crystal_cap_reg(dev, cap);
    }
}

/// Upstream `rtw8822c_cfo_init`.
pub fn cfo_init<H: Hci>(dev: &mut Dev<H>) {
    dev.dm.crystal_cap = dev.efuse.crystal_cap;
    dev.dm.cfo_is_adjust = true;
}
