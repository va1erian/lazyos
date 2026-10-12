//! The 8822C's coexistence hooks (upstream `rtw8822c_coex_cfg_init`,
//! `rtw8822c_coex_cfg_gnt_debug`, `rtw8822c_coex_cfg_rfe_type`,
//! `rtw8822c_coex_cfg_wl_rx_gain`) and its PTA tables (`table_sant_8822c`,
//! `table_nsant_8822c`).

use crate::coex::write_indirect_reg;
use crate::dev::{Dev, RF_PATH_A, RF_PATH_B};
use crate::hci::{Hci, RegExt};
use crate::phy::rf::{write_rf, RFREG_MASK};
use crate::rtw8822c::regs::*;
use crate::rtw8822c::util::{clr8, set8, write8_mask};

/// Upstream `table_sant_8822c`: `[bt, wl]` per case 0..37.
pub const TABLE_SANT: [[u32; 2]; 38] = [
    [0xffffffff, 0xffffffff],
    [0x55555555, 0x55555555],
    [0x66555555, 0x66555555],
    [0xaaaaaaaa, 0xaaaaaaaa],
    [0x5a5a5a5a, 0x5a5a5a5a],
    [0xfafafafa, 0xfafafafa],
    [0x6a5a5555, 0xaaaaaaaa],
    [0x6a5a56aa, 0x6a5a56aa],
    [0x6a5a5a5a, 0x6a5a5a5a],
    [0x66555555, 0x5a5a5a5a],
    [0x66555555, 0x6a5a5a5a],
    [0x66555555, 0x6a5a5aaa],
    [0x66555555, 0x5a5a5aaa],
    [0x66555555, 0x6aaa5aaa],
    [0x66555555, 0xaaaa5aaa],
    [0x66555555, 0xaaaaaaaa],
    [0xffff55ff, 0xfafafafa],
    [0xffff55ff, 0x6afa5afa],
    [0xaaffffaa, 0xfafafafa],
    [0xaa5555aa, 0x5a5a5a5a],
    [0xaa5555aa, 0x6a5a5a5a],
    [0xaa5555aa, 0xaaaaaaaa],
    [0xffffffff, 0x5a5a5a5a],
    [0xffffffff, 0x5a5a5a5a],
    [0xffffffff, 0x55555555],
    [0xffffffff, 0x5a5a5aaa],
    [0x55555555, 0x5a5a5a5a],
    [0x55555555, 0xaaaaaaaa],
    [0x55555555, 0x6a5a6a5a],
    [0x66556655, 0x66556655],
    [0x66556aaa, 0x6a5a6aaa],
    [0xffffffff, 0x5aaa5aaa],
    [0x56555555, 0x5a5a5aaa],
    [0xdaffdaff, 0xdaffdaff],
    [0xddffddff, 0xddffddff],
    [0xe5555555, 0xe5555555],
    [0xea5a5a5a, 0xea5a5a5a],
    [0xea6a6a6a, 0xea6a6a6a],
];

/// Upstream `table_nsant_8822c`: `[bt, wl]` per case 100..123.
pub const TABLE_NSANT: [[u32; 2]; 24] = [
    [0xffffffff, 0xffffffff],
    [0x55555555, 0x55555555],
    [0x66555555, 0x66555555],
    [0xaaaaaaaa, 0xaaaaaaaa],
    [0x5a5a5a5a, 0x5a5a5a5a],
    [0xfafafafa, 0xfafafafa],
    [0x5afa5afa, 0x5afa5afa],
    [0x55555555, 0xfafafafa],
    [0x66555555, 0xfafafafa],
    [0x66555555, 0x5a5a5a5a],
    [0x66555555, 0x6a5a5a5a],
    [0x66555555, 0xaaaaaaaa],
    [0xffff55ff, 0xfafafafa],
    [0xffff55ff, 0x5afa5afa],
    [0xffff55ff, 0xaaaaaaaa],
    [0xffff55ff, 0xffff55ff],
    [0xaaffffaa, 0x5afa5afa],
    [0xaaffffaa, 0xaaaaaaaa],
    [0xffffffff, 0xfafafafa],
    [0xffffffff, 0x5afa5afa],
    [0xffffffff, 0xaaaaaaaa],
    [0x55ff55ff, 0x5afa5afa],
    [0x55ff55ff, 0xaaaaaaaa],
    [0x55ff55ff, 0x55ff55ff],
];

/// Upstream `rtw8822c_coex_cfg_init`: TBTT interrupt, BT report sample
/// rate and counters, the PTA (3-wire) enabled from both sides, GNT_BT
/// polarity, BT CCA from GNT_WL, and RF B register 1 set "to avoid RF
/// parameter error".
pub fn cfg_init<H: Hci>(dev: &mut Dev<H>) {
    let h = &mut dev.hci;
    set8(h, REG_BCN_CTRL, BIT_EN_BCN_FUNCTION);
    write8_mask(h, REG_BT_TDMA_TIME, BIT_MASK_SAMPLE_RATE, 0x5);
    h.write8(REG_BT_STAT_CTRL, 0x1);
    h.set32(REG_GPIO_MUXCFG, BIT_BT_PTA_EN);
    h.set32(REG_GPIO_MUXCFG, BIT_PO_BT_PTA_PINS);
    set8(h, REG_QUEUE_CTRL, BIT_PTA_WL_TX_EN);
    clr8(h, REG_QUEUE_CTRL, BIT_PTA_EDCCA_EN);
    h.set16(REG_BT_COEX_V2, BIT_GNT_BT_POLARITY as u16);
    clr8(h, REG_DUMMY_PAGE4_V1, BIT_BTCCA_CTRL);
    write_rf(dev, RF_PATH_B, RF_MODOPT, 0xfffff, 0x40000);
}

/// Upstream `rtw8822c_coex_cfg_gnt_debug`: the BT GPIO/JTAG/SPI and LED1
/// pins back to normal, grant debug output off.
pub fn cfg_gnt_debug<H: Hci>(dev: &mut Dev<H>) {
    let h = &mut dev.hci;
    write8_mask(h, REG_PAD_CTRL1 + 2, BIT_BTGP_SPI_EN >> 16, 0);
    write8_mask(h, REG_PAD_CTRL1 + 3, BIT_BTGP_JTAG_EN >> 24, 0);
    write8_mask(h, REG_GPIO_MUXCFG + 2, BIT_FSPI_EN >> 16, 0);
    write8_mask(h, REG_PAD_CTRL1 + 1, BIT_LED1DIS >> 8, 0);
    write8_mask(h, REG_SYS_SDIO_CTRL + 3, BIT_DBG_GNT_WL_BT >> 24, 0);
}

/// Upstream `rtw8822c_coex_cfg_rfe_type`: no antenna switch on any 8822C
/// RFE type; Wi-Fi 2.4 GHz shares Bluetooth's path when the efuse says the
/// antenna is shared; LTE coexistence off.
pub fn cfg_rfe_type<H: Hci>(dev: &mut Dev<H>) {
    dev.coex.rfe_module_type = dev.efuse.rfe_option;
    dev.coex.wlg_at_btg = dev.efuse.share_ant;
    write_indirect_reg(dev, LTE_COEX_CTRL, BIT_LTE_COEX_EN, 0x0);
    write_indirect_reg(dev, LTE_WL_TRX_CTRL, MASKLWORD, 0xffff);
    write_indirect_reg(dev, LTE_BT_TRX_CTRL, MASKLWORD, 0xffff);
}

/// Upstream `rtw8822c_coex_cfg_wl_rx_gain`: the RX filter corner offset
/// for the "Hi-Li" low-gain table, on a change.
pub fn cfg_wl_rx_gain<H: Hci>(dev: &mut Dev<H>, low_gain: bool) {
    if dev.coex.cur_wl_rx_low_gain_en == low_gain {
        return;
    }
    dev.coex.cur_wl_rx_low_gain_en = low_gain;
    let (rckd, rck) = if low_gain { (0x22, 0x36) } else { (0x20, 0x0) };
    for path in [RF_PATH_A, RF_PATH_B] {
        write_rf(dev, path, RF_RCKD, RFREG_MASK, rckd);
        write_rf(dev, path, RF_RCK, RFREG_MASK, rck);
    }
}
