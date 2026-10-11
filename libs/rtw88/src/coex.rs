//! Bluetooth coexistence, the minimum (upstream `coex.c`).
//!
//! Owner: the chip work package. LazyOS has no Bluetooth stack, so this is
//! only what the chip needs to give the shared antenna and the RF switch to
//! Wi-Fi: `rtw_coex_power_on_setting`, `rtw_coex_init_hw_config(wifi_only)`,
//! `rtw_coex_power_off_setting`, and the `rtw8822c_coex_cfg_*` hooks those
//! call. No coexistence mechanism (TDMA tables, BT profiles) runs.
//!
//! Contract used by [`crate::bringup`]:
//! `power_on_setting(dev)`, `init_hw_config(dev, wifi_only)`,
//! `power_off_setting(dev)`.

/// What the coex code keeps between calls.
#[derive(Clone, Debug, Default)]
pub struct CoexState {}
