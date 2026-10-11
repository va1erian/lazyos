//! The RTL8822C's logical efuse layout (upstream `struct rtw8822c_efuse`
//! and `struct rtw8822ce_efuse` in `rtw8822c.h`, `rtw8822c_read_efuse` and
//! `rtw8822ce_efuse_parsing` in `rtw8822c.c`).
//!
//! | Offset | Field |
//! |---|---|
//! | 0x00 | `rtl_id` (le16) |
//! | 0x06 | `usb_mode` |
//! | 0x10 | TX power index tables, 4 paths x 42 bytes |
//! | 0xb8 | `channel_plan` |
//! | 0xb9 | `xtal_k` (crystal cap, low 7 bits) |
//! | 0xc1 | `rf_board_option` (`regd` in bits 2:0) |
//! | 0xc3 | `rf_bt_setting` |
//! | 0xc8 | `tx_pwr_calibrate_rate` (power tracking type in 7:4) |
//! | 0xca | `rfe_option` |
//! | 0xcb | `country_code[2]` |
//! | 0xd0 | `path_a_thermal`, 0xd1 `path_b_thermal` |
//! | 0x120 | PCIe: `mac_addr[6]` (`rtw8822ce_efuse`) |

use crate::dev::{Dev, RF_PATH_MAX};
use crate::efuse::Efuse;
use crate::error::{Error, Result};
use crate::hci::Hci;
use crate::phy::txpwr::TxPwrIdx;

const TXPWR_IDX_TABLE: usize = 0x10;
const TXPWR_IDX_LEN: usize = 42;
const CHANNEL_PLAN: usize = 0xb8;
const XTAL_K: usize = 0xb9;
const RF_BOARD_OPTION: usize = 0xc1;
const RF_BT_SETTING: usize = 0xc3;
const TX_PWR_CALIBRATE_RATE: usize = 0xc8;
const RFE_OPTION: usize = 0xca;
const COUNTRY_CODE: usize = 0xcb;
const PATH_A_THERMAL: usize = 0xd0;
const PATH_B_THERMAL: usize = 0xd1;
const PCIE_MAC_ADDR: usize = 0x120;
/// The logical map must reach the end of the PCIe MAC address.
pub const MIN_LOG_MAP: usize = PCIE_MAC_ADDR + 6;

/// Upstream `XCAP_MASK`.
pub const XCAP_MASK: u8 = 0x7f;

/// Upstream `rtw8822c_read_efuse` for a PCIe card: fill `efuse` from the
/// logical map. The defaults upstream applies afterwards for unprogrammed
/// fields (`rtw_chip_efuse_info_setup`: 0xff crystal cap, channel plan,
/// board option, `share_ant`, `btcoex`, ...) belong to the core and are not
/// applied here. The TX power tables go to [`read_txpwr_idx`].
pub fn read_efuse(efuse: &mut Efuse, log_map: &[u8]) -> Result<()> {
    if log_map.len() < MIN_LOG_MAP {
        return Err(Error::Efuse("logical map too short"));
    }
    let m = log_map;
    efuse.rfe_option = m[RFE_OPTION];
    efuse.rf_board_option = m[RF_BOARD_OPTION];
    efuse.crystal_cap = m[XTAL_K] & XCAP_MASK;
    efuse.channel_plan = m[CHANNEL_PLAN];
    efuse.country_code = [m[COUNTRY_CODE], m[COUNTRY_CODE + 1]];
    efuse.bt_setting = m[RF_BT_SETTING];
    efuse.regd = m[RF_BOARD_OPTION] & 0x7;
    efuse.thermal_meter[0] = m[PATH_A_THERMAL];
    efuse.thermal_meter[1] = m[PATH_B_THERMAL];
    efuse.thermal_meter_k = ((u16::from(m[PATH_A_THERMAL]) + u16::from(m[PATH_B_THERMAL])) >> 1) as u8;
    efuse.power_track_type = (m[TX_PWR_CALIBRATE_RATE] >> 4) & 0xf;
    // rtw8822ce_efuse_parsing
    efuse.addr.copy_from_slice(&m[PCIE_MAC_ADDR..PCIE_MAC_ADDR + 6]);
    Ok(())
}

/// The TX power index tables of the paths this driver drives (upstream
/// copies all four into `efuse->txpwr_idx_table`; the 8822C has two).
pub fn read_txpwr_idx(log_map: &[u8]) -> Result<[TxPwrIdx; RF_PATH_MAX]> {
    let mut out = [TxPwrIdx::default(); RF_PATH_MAX];
    for (path, t) in out.iter_mut().enumerate() {
        let start = TXPWR_IDX_TABLE + path * TXPWR_IDX_LEN;
        let bytes = log_map
            .get(start..start + TXPWR_IDX_LEN)
            .ok_or(Error::Efuse("logical map too short"))?;
        let mut b = [0u8; TXPWR_IDX_LEN];
        b.copy_from_slice(bytes);
        *t = TxPwrIdx::parse(&b);
    }
    Ok(out)
}

/// Both of the above into the device: [`read_efuse`] into `dev.efuse` and
/// the power tables into `dev.phy`.
pub fn efuse_setup<H: Hci>(dev: &mut Dev<H>, log_map: &[u8]) -> Result<()> {
    read_efuse(&mut dev.efuse, log_map)?;
    dev.phy.txpwr_idx = read_txpwr_idx(log_map)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_map() -> std::vec::Vec<u8> {
        let mut m = std::vec![0xffu8; 768];
        m[0] = 0x29;
        m[1] = 0x81; // rtl_id 0x8129
        // Path A power: CCK base 0x2a.., path B: 0x30..
        for i in 0..6 {
            m[0x10 + i] = 0x2a + i as u8;
            m[0x10 + 42 + i] = 0x30 + i as u8;
        }
        m[0x10 + 11] = 0xe1; // path A ht_1s_2g: ofdm +1, bw20 -2
        m[CHANNEL_PLAN] = 0x7f;
        m[XTAL_K] = 0xbc; // bit 7 is not part of the cap
        m[RF_BOARD_OPTION] = 0x21; // btcoex in 7:5, regd 1
        m[RF_BT_SETTING] = 0x01;
        m[TX_PWR_CALIBRATE_RATE] = 0x20;
        m[RFE_OPTION] = 0x05;
        m[COUNTRY_CODE] = b'F';
        m[COUNTRY_CODE + 1] = b'R';
        m[PATH_A_THERMAL] = 0x1a;
        m[PATH_B_THERMAL] = 0x1d;
        m[0x120..0x126].copy_from_slice(&[0x00, 0xe0, 0x4c, 0x88, 0x22, 0xc0]);
        m
    }

    #[test]
    fn parses_the_8822ce_layout() {
        let m = synthetic_map();
        let mut e = Efuse::default();
        read_efuse(&mut e, &m).unwrap();
        assert_eq!(e.addr, [0x00, 0xe0, 0x4c, 0x88, 0x22, 0xc0]);
        assert_eq!(e.rfe_option, 5);
        assert_eq!(e.crystal_cap, 0x3c);
        assert_eq!(e.channel_plan, 0x7f);
        assert_eq!(e.country_code, *b"FR");
        assert_eq!(e.rf_board_option, 0x21);
        assert_eq!(e.regd, 1);
        assert_eq!(e.bt_setting, 1);
        assert_eq!(e.thermal_meter, [0x1a, 0x1d]);
        assert_eq!(e.thermal_meter_k, 0x1b);
        assert_eq!(e.power_track_type, 2);

        let t = read_txpwr_idx(&m).unwrap();
        assert_eq!(t[0].cck_base_2g, [0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f]);
        assert_eq!(t[1].cck_base_2g[0], 0x30);
        assert_eq!((t[0].ht_1s_2g.ofdm, t[0].ht_1s_2g.bw20), (1, -2));
    }

    #[test]
    fn refuses_a_short_map() {
        let m = synthetic_map();
        let mut e = Efuse::default();
        assert!(read_efuse(&mut e, &m[..0x125]).is_err());
        assert!(read_efuse(&mut e, &[]).is_err());
        assert!(read_txpwr_idx(&m[..0x10 + 83]).is_err());
    }
}
