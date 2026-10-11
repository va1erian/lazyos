//! The PHY (upstream `phy.c`, `phy.h`): BB/AGC/RF/MAC table loading with
//! their condition headers (`rtw_parse_tbl_*`, `check_positive`), RF register
//! access over SIPI (`rtw_phy_read_rf`, `rtw_phy_write_rf_reg_mix`), and TX
//! power (by-rate and limit tables, `rtw_phy_set_tx_power_level`).
//!
//! Owner: the chip work package.
//!
//! Contract: `read_rf(dev, path, addr, mask) -> u32`,
//! `write_rf(dev, path, addr, mask, data)`.

/// PHY state the chip code keeps (tx power tables, table conditions).
#[derive(Clone, Debug, Default)]
pub struct PhyState {}
