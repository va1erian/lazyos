//! The RTL8822C itself (upstream `rtw8822c.c`, `rtw8822c.h`,
//! `rtw8822c_table.c`, `rtw8822ce.c`).
//!
//! Owners: [`spec`] (the chip constants and tables the MAC and PCIe code
//! read: descriptor sizes, FIFO sizes, page and queue tables, priority queue
//! addresses, PCIe PHY parameters, power sequences) belongs to the core/MAC
//! work package; everything else here to the chip work package.
//!
//! Contract used by [`crate::bringup`] and [`crate::rx`] (upstream
//! `rtw8822c_ops`):
//!
//! | Here | Upstream |
//! |---|---|
//! | [`read_efuse`]`(&mut Efuse, log_map)` | `rtw8822c_read_efuse` |
//! | [`phy_set_param`]`(dev)` | `rtw8822c_phy_set_param` |
//! | [`mac_init`]`(dev)` | `rtw8822c_mac_init` |
//! | [`set_channel`]`(dev, channel, bw, primary_chan_idx)` | `rtw8822c_set_channel` |
//! | [`query_phy_status`]`(&ctx, phy_status, &mut RxPktStat)`, ctx from [`phy_status_ctx`]`(dev)` | `query_phy_status` |
//! | [`set_tx_power_index`]`(dev)` (called by `phy::set_tx_power_level`) | `rtw8822c_set_tx_power_index` |
//! | [`phy_calibration`]`(dev)` | `rtw8822c_phy_calibration` |
//! | [`config_trx_mode`], [`set_antenna`], [`cfg_ldo25`], [`cfo_init`], [`false_alarm_statistics`] | the ops of the same names |
//!
//! What this chip code calls in other packages: `crate::fw` (H2C commands,
//! IQK), `crate::mac::set_channel_mac`, `crate::efuse::read8_physical_efuse`
//! and the LTE coex window in `crate::mac`.

pub mod calib;
pub mod channel;
pub mod dack;
pub mod dack_restore;
pub mod dpk;
pub mod dpk_agc;
pub mod efuse;
pub mod macinit;
pub mod param;
pub mod phystatus;
pub mod regs;
pub mod spec;
pub mod tables;
pub mod trim;
pub mod trx;
pub mod txpower;
pub mod util;

#[cfg(test)]
mod tests_channel;
#[cfg(test)]
mod tests_param;
#[cfg(test)]
mod tests_phystatus;

pub use calib::{false_alarm_statistics, phy_calibration, FalseAlarm};
pub use channel::set_channel;
pub use efuse::read_efuse;
pub use macinit::mac_init;
pub use param::{cfg_ldo25, cfo_init, phy_set_param};
pub use phystatus::{phy_status_ctx, query_phy_status, PhyStatusCtx};
pub use trx::{config_trx_mode, set_antenna};
pub use txpower::set_tx_power_index;

use crate::dev::RF_PATH_MAX;
use dack_restore::DackState;
use dpk::DpkInfo;

/// Dynamic mechanism and calibration state (upstream `struct rtw_dm_info`,
/// the part this chip uses: DACK and DPK results, power tracking, CFO, RX
/// statistics).
#[derive(Clone, Debug, Default)]
pub struct DmState {
    /// The CCK AGC gain-index window (`cck_gi_l_bnd`, `cck_gi_u_bnd`).
    pub cck_gi_l_bnd: u8,
    pub cck_gi_u_bnd: u8,
    pub dack: DackState,
    pub dpk: DpkInfo,
    pub iqk_done: bool,
    /// `igi_history[0]`: the initial gain after the tables load.
    pub igi_history0: u8,
    pub delta_power_index: [u8; RF_PATH_MAX],
    pub thermal_avg: [u8; RF_PATH_MAX],
    pub pwr_trk_triggered: bool,
    pub thermal_meter_k: u8,
    pub thermal_meter_lck: u8,
    /// `cfo_track.crystal_cap` / `is_adjust`.
    pub crystal_cap: u8,
    pub cfo_is_adjust: bool,
    /// Per-path RX statistics (`rssi`, `rx_snr`, `cfo_tail`, `rx_evm_dbm`).
    pub rssi: [u8; RF_PATH_MAX],
    pub rx_snr: [i8; RF_PATH_MAX],
    pub cfo_tail: [i16; RF_PATH_MAX],
    pub rx_evm_dbm: [u8; RF_PATH_MAX],
    pub curr_rx_rate: u8,
    pub fa: FalseAlarm,
}
