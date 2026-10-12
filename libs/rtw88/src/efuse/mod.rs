//! The efuse (upstream `efuse.c` for reading, `struct rtw_efuse` in
//! `main.h` for the result, `main.c` `rtw_dump_hw_feature` for the
//! hardware capabilities the firmware reports).
//!
//! Owner of the reading code: the core/MAC work package ([`read`]:
//! physical efuse to logical map, `rtw_parse_efuse_map`). Owner of the
//! layout: the chip work package (`crate::rtw8822c::read_efuse`, upstream
//! `rtw8822c_read_efuse`), which fills [`Efuse`] from the logical map.

pub mod read;

#[cfg(test)]
mod tests;

pub use read::{dump_hw_feature, logical_map, parse_efuse_map, read8_physical_efuse};

/// Bytes of one `struct rtw_txpwr_idx` as stored in the efuse: the 2.4 GHz
/// part (CCK and BW40 bases for 6 and 5 groups, HT 1-4SS diffs: 18 bytes)
/// and the 5 GHz part (14 BW40 bases, HT, OFDM and VHT diffs: 24 bytes).
pub const TXPWR_IDX_LEN: usize = 42;

/// One path's TX power index table, raw (`struct rtw_txpwr_idx`, packed
/// nibbles). The chip/PHY code decodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxPwrIdx(pub [u8; TXPWR_IDX_LEN]);

impl Default for TxPwrIdx {
    fn default() -> TxPwrIdx {
        TxPwrIdx([0; TXPWR_IDX_LEN])
    }
}

/// `EFUSE_HW_CAP_*`.
pub const HW_CAP_IGNORE: u8 = 0;
pub const HW_CAP_PTCL_VHT: u8 = 3;
pub const HW_CAP_SUPP_BW80: u8 = 7;
pub const HW_CAP_SUPP_BW40: u8 = 6;

/// What the firmware's hardware feature report says (upstream
/// `efuse->hw_cap`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HwCap {
    pub hci: u8,
    /// Bitmap of supported widths, bit `Bw as u8` set when supported.
    pub bw: u8,
    pub ptcl: u8,
    /// Spatial streams, clamped to the RF paths.
    pub nss: u8,
    pub ant_num: u8,
}

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

    pub usb_mode_switch: u8,
    pub hw_cap: HwCap,
    /// Per-path TX power indexes (`txpwr_idx_table[4]`), raw.
    pub txpwr_idx_table: [TxPwrIdx; 4],
}

impl Efuse {
    /// The tail of upstream `rtw_chip_efuse_info_setup`: replace erased
    /// (`0xff`) fields with defaults and derive the flags. Returns `false`
    /// when the efuse address is not a usable unicast MAC address (upstream
    /// then makes up a random one; the caller decides, it has the RNG).
    pub fn apply_defaults(&mut self) -> bool {
        fn erased(v: &mut u8, default: u8) {
            if *v == 0xff {
                *v = default;
            }
        }
        erased(&mut self.crystal_cap, 0);
        erased(&mut self.pa_type_2g, 0);
        erased(&mut self.pa_type_5g, 0);
        erased(&mut self.lna_type_2g, 0);
        erased(&mut self.lna_type_5g, 0);
        erased(&mut self.channel_plan, 0x7f);
        erased(&mut self.rf_board_option, 0);
        if self.bt_setting & 1 != 0 {
            self.share_ant = true;
        }
        erased(&mut self.regd, 0);
        erased(&mut self.tx_bb_swing_setting_2g, 0);
        erased(&mut self.tx_bb_swing_setting_5g, 0);

        self.btcoex = (self.rf_board_option & 0xe0) == 0x20;
        self.ext_pa_2g = self.pa_type_2g & (1 << 4) != 0;
        self.ext_lna_2g = self.lna_type_2g & (1 << 3) != 0;
        self.ext_pa_5g = self.pa_type_5g & (1 << 0) != 0;
        self.ext_lna_5g = self.lna_type_5g & (1 << 3) != 0;

        is_valid_ether_addr(&self.addr)
    }
}

/// Upstream `is_valid_ether_addr`: not multicast, not all zeros.
pub fn is_valid_ether_addr(a: &[u8; 6]) -> bool {
    a[0] & 1 == 0 && a.iter().any(|&b| b != 0)
}
