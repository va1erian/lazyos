//! The PHY (upstream `phy.c`, `phy.h`): BB/AGC/RF/MAC table loading with
//! their condition headers ([`cond`]: `rtw_parse_tbl_phy_cond`,
//! `check_positive`), RF register access ([`rf`]: `rtw_phy_read_rf`,
//! `rtw_phy_write_rf_reg_mix`), TX power ([`txpwr`]: the by-rate and limit
//! tables, `rtw_phy_get_tx_power_index`, `rtw_phy_set_tx_power_level`), and
//! the RF power to RSSI conversion the RX path uses ([`rssi`]).
//!
//! Owner: the chip work package.
//!
//! Contract: [`read_rf`]`(dev, path, addr, mask) -> u32`,
//! [`write_rf`]`(dev, path, addr, mask, data)`, and for the bring-up
//! [`setup_board_info`] (upstream `rtw_chip_board_info_setup`: the table
//! condition and the TX power tables, after the efuse is read) and
//! [`txpwr::set_tx_power_level`] after each channel change.

pub mod cond;
pub mod dig;
pub mod rate;
pub mod rf;
pub mod rssi;
pub mod txpwr;

#[cfg(test)]
mod tests_cond;
#[cfg(test)]
mod tests_txpwr;

pub use cond::{load_tables, PhyCond};
pub use dig::{dig_reset, dig_set_max_coverage};
pub use rf::{read_rf, write_rf};
pub use txpwr::{set_tx_power_level, TxPwrIdx};

use crate::dev::{Dev, RF_PATH_MAX};
use crate::error::{Error, Result};
use crate::hci::Hci;
use rate::{DESC_RATE_MAX, RATE_SECTION_NUM};

/// Upstream `RTW_CHANNEL_WIDTH_MAX`: the widths the limit tables cover.
pub const BW_MAX: usize = 3;
/// Upstream `RTW_MAX_CHANNEL_NUM_2G`.
pub const MAX_CHANNEL_NUM_2G: usize = 14;
/// Upstream `RTW_MAX_CHANNEL_NUM_5G`.
pub const MAX_CHANNEL_NUM_5G: usize = 49;
/// Upstream `chip->max_power_index` for the 8822C.
pub const MAX_POWER_INDEX: u8 = 0x7f;
/// Upstream `chip->txgi_factor` for the 8822C.
pub const TXGI_FACTOR: u8 = 2;
/// Upstream `RTW_REGD_WW`, the only limit set this driver keeps
/// ([`PhyState::limit_2g`]).
pub const REGD_WW: u8 = 12;

/// Upstream `IS_CH_2G_BAND`.
pub fn is_ch_2g(channel: u8) -> bool {
    channel <= 14
}

/// Upstream `IS_CH_5G_BAND_1` to `_4` and `IS_CH_5G_BAND`.
pub fn ch_5g_band(channel: u8) -> Option<u8> {
    match channel {
        36..=48 => Some(1),
        52..=64 => Some(2),
        100..=144 => Some(3),
        149..=177 => Some(4),
        _ => None,
    }
}

/// What the PHY keeps (the parts of upstream `struct rtw_hal` that hold TX
/// power, plus the table condition).
///
/// **One regulatory domain.** Upstream keeps limit tables for all 13
/// domains and picks one from the country the stack reports. LazyOS has no
/// country yet, so only the world-wide set (`RTW_REGD_WW`) is kept: upstream
/// builds it as the minimum over every domain in the table, which makes it
/// the safest choice anywhere (no lower than ETSI's or FCC's own rows).
#[derive(Clone, Debug)]
pub struct PhyState {
    /// The condition the tables are matched against (`hal->phy_cond`).
    pub cond: PhyCond,
    /// TX power index tables from the efuse, one per RF path
    /// (`efuse->txpwr_idx_table`).
    pub txpwr_idx: [TxPwrIdx; RF_PATH_MAX],
    /// `tx_pwr_by_rate_offset_2g/5g`: per path and rate, relative to the
    /// section base after [`txpwr::by_rate_config`].
    pub by_rate_2g: [[i8; DESC_RATE_MAX]; RF_PATH_MAX],
    pub by_rate_5g: [[i8; DESC_RATE_MAX]; RF_PATH_MAX],
    /// `tx_pwr_by_rate_base_2g/5g`: per path and rate section.
    pub by_rate_base_2g: [[i8; RATE_SECTION_NUM]; RF_PATH_MAX],
    pub by_rate_base_5g: [[i8; RATE_SECTION_NUM]; RF_PATH_MAX],
    /// `tx_pwr_limit_2g/5g[RTW_REGD_WW]`: per width, rate section, channel index.
    pub limit_2g: [[[i8; MAX_CHANNEL_NUM_2G]; RATE_SECTION_NUM]; BW_MAX],
    pub limit_5g: [[[i8; MAX_CHANNEL_NUM_5G]; RATE_SECTION_NUM]; BW_MAX],
    /// `hal->tx_pwr_tbl`: the index each rate transmits at on the current
    /// channel, written to the chip by `rtw8822c::set_tx_power_index`.
    pub tx_pwr_tbl: [[u8; DESC_RATE_MAX]; RF_PATH_MAX],
    /// Set once [`setup_board_info`] has loaded the power tables.
    pub tables_ready: bool,
}

impl Default for PhyState {
    fn default() -> Self {
        let max = MAX_POWER_INDEX as i8;
        PhyState {
            cond: PhyCond::default(),
            txpwr_idx: [TxPwrIdx::default(); RF_PATH_MAX],
            by_rate_2g: [[0; DESC_RATE_MAX]; RF_PATH_MAX],
            by_rate_5g: [[0; DESC_RATE_MAX]; RF_PATH_MAX],
            by_rate_base_2g: [[0; RATE_SECTION_NUM]; RF_PATH_MAX],
            by_rate_base_5g: [[0; RATE_SECTION_NUM]; RF_PATH_MAX],
            limit_2g: [[[max; MAX_CHANNEL_NUM_2G]; RATE_SECTION_NUM]; BW_MAX],
            limit_5g: [[[max; MAX_CHANNEL_NUM_5G]; RATE_SECTION_NUM]; BW_MAX],
            tx_pwr_tbl: [[0; DESC_RATE_MAX]; RF_PATH_MAX],
            tables_ready: false,
        }
    }
}

/// Upstream `rtw_chip_board_info_setup`, after the efuse is parsed: build
/// the table condition (`rtw_phy_setup_phy_cond`) from the cut, package and
/// RFE type, then load the RFE type's by-rate and limit tables
/// (`rtw_phy_init_tx_power`, `rtw_parse_tbl_bb_pg`,
/// `rtw_parse_tbl_txpwr_lmt`, `rtw_phy_tx_power_by_rate_config`,
/// `rtw_phy_tx_power_limit_config`), after decoding the efuse's raw power
/// index tables into [`PhyState::txpwr_idx`]. Refuses an RFE type upstream has no
/// definition for (`rtw_check_supported_rfe`).
pub fn setup_board_info<H: Hci>(dev: &mut Dev<H>) -> Result<()> {
    let rfe = crate::rtw8822c::tables::rfe_def(dev.efuse.rfe_option)
        .ok_or(Error::Unsupported("rfe type"))?;
    for (decoded, raw) in dev
        .phy
        .txpwr_idx
        .iter_mut()
        .zip(dev.efuse.txpwr_idx_table.iter())
    {
        *decoded = TxPwrIdx::parse(&raw.0);
    }
    dev.phy.cond = PhyCond::setup(dev.hal.cut_version, dev.hal.pkg_type, dev.efuse.rfe_option);
    txpwr::init_tx_power(&mut dev.phy);
    txpwr::parse_bb_pg(dev, rfe.phy_pg);
    txpwr::parse_txpwr_lmt(&mut dev.phy, rfe.txpwr_lmt);
    txpwr::by_rate_config(&mut dev.phy);
    txpwr::limit_config(&mut dev.phy);
    // Upstream's `rtw_phy_tx_power_limit_config` also points the 20 MHz
    // centre channel at 1 until the first channel switch.
    dev.hal.cch_by_bw[0] = 1;
    dev.phy.tables_ready = true;
    Ok(())
}
