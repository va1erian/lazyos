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
//! `read_efuse(&mut Efuse, log_map: &[u8]) -> Result<()>`,
//! `phy_set_param(dev) -> Result<()>`, `mac_init(dev) -> Result<()>`,
//! `set_channel(dev, channel, bw, primary_chan_idx)`,
//! `query_phy_status(phy_status: &[u8], &mut RxPktStat)`,
//! `set_tx_power_index(dev)`, `phy_calibration(dev)`.

pub mod spec;

/// Dynamic mechanism and calibration state (upstream `struct rtw_dm_info`,
/// the part this chip uses: DACK, IQK, DPK results, thermal tracking).
#[derive(Clone, Debug, Default)]
pub struct DmState {}
