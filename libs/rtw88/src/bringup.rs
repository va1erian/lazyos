//! The bring-up order (upstream `main.c`: `rtw_power_on`, `rtw_core_start`,
//! `rtw_power_off`, `rtw_chip_info_setup`, `rtw_chip_efuse_info_setup`).
//!
//! Owner: the core/MAC work package. This is the one place that knows the
//! order; every step it calls lives in its own module.
