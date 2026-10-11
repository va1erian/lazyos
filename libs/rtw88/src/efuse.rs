//! The efuse (upstream `efuse.c` for reading, `struct rtw_efuse` in
//! `main.h` for the result).
//!
//! Owner of the reading code: the core/MAC work package (physical efuse to
//! logical map, `rtw_parse_efuse_map`). Owner of the layout: the chip work
//! package ([`crate::rtw8822c::efuse`], upstream `rtw8822c_read_efuse`),
//! which fills [`Efuse`] from the logical map.

/// What the efuse says about this card (upstream `struct rtw_efuse`, the
/// fields this driver uses; extend in upstream order).
#[derive(Clone, Debug, Default)]
pub struct Efuse {
    pub physical_size: u32,
    pub logical_size: u32,
    pub protect_size: u32,

    /// The station address.
    pub addr: [u8; 6],
    pub channel_plan: u8,
    pub country_code: [u8; 2],
    pub rf_board_option: u8,
    pub rfe_option: u8,
    pub power_track_type: u8,
    pub thermal_meter: [u8; crate::dev::RF_PATH_MAX],
    pub thermal_meter_k: u8,
    pub crystal_cap: u8,
    pub ant_div_cfg: u8,
    pub ant_div_type: u8,
    pub regd: u8,
    pub afe: u8,

    pub lna_type_2g: u8,
    pub lna_type_5g: u8,
    pub glna_type: u8,
    pub alna_type: u8,
    pub ext_lna_2g: bool,
    pub ext_lna_5g: bool,
    pub pa_type_2g: u8,
    pub pa_type_5g: u8,
    pub gpa_type: u8,
    pub apa_type: u8,
    pub ext_pa_2g: bool,
    pub ext_pa_5g: bool,
    pub tx_bb_swing_setting_2g: u8,
    pub tx_bb_swing_setting_5g: u8,

    /// Bluetooth coexistence is configured on this card.
    pub btcoex: bool,
    /// Bluetooth shares the antenna with Wi-Fi.
    pub share_ant: bool,
    pub bt_setting: u8,
}
