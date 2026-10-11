//! Table loading with condition headers (upstream `phy.c`:
//! `rtw_phy_setup_phy_cond`, `check_positive`, `rtw_parse_tbl_phy_cond`,
//! `rtw_phy_cfg_mac/agc/bb/rf`, `rtw_load_rfk_table`, `rtw_phy_load_tables`).
//!
//! A table is a stream of `(addr, data)` word pairs. A pair whose address has
//! bit 31 set (`pos`) opens or continues an `if`/`elif`/`else`/`endif` chain
//! and carries the condition in its address word (`struct rtw_phy_cond`); a
//! pair with bit 30 set (`neg`) closes the condition and decides it. Every
//! other pair is a register write, applied while the current branch matches.
//! The bit layout (little-endian bitfields) is:
//!
//! | bits | 31 | 30 | 29:28 | 27:24 | 23:20 | 19:16 | 15:12 | 11:8 | 7:0 |
//! |---|---|---|---|---|---|---|---|---|---|
//! | field | pos | neg | branch | cut | intf_rsvd | plat | pkg | intf | rfe |

use crate::dev::Dev;
use crate::hci::{Hci, RegExt};
use crate::phy::rf::{write_rf, RFREG_MASK};
use crate::rtw8822c::tables;

/// Upstream `INTF_PCIE` (bit 0 of `intf`).
pub const INTF_PCIE: u8 = 1 << 0;
const BRANCH_ELSE: u32 = 2;
const BRANCH_ENDIF: u32 = 3;

/// The hardware a table branch is written for (upstream `struct
/// rtw_phy_cond`; the 8822C never uses `struct rtw_phy_cond2`, which only
/// the 8812A/8821A read).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhyCond {
    pub rfe: u8,
    pub intf: u8,
    pub pkg: u8,
    pub plat: u8,
    pub cut: u8,
}

impl PhyCond {
    /// Upstream `rtw_phy_setup_phy_cond`: cut A (0) and "no package" (0) are
    /// written as 15 so that a header's 0 can mean "any"; the platform is
    /// always 4 and the interface PCIe.
    pub fn setup(cut_version: u8, pkg: u8, rfe_option: u8) -> PhyCond {
        PhyCond {
            cut: if cut_version != 0 { cut_version } else { 15 },
            pkg: if pkg != 0 { pkg } else { 15 },
            plat: 0x04,
            rfe: rfe_option,
            intf: INTF_PCIE,
        }
    }

    /// The condition fields of a header word.
    pub fn from_word(w: u32) -> PhyCond {
        PhyCond {
            rfe: w as u8,
            intf: ((w >> 8) & 0xf) as u8,
            pkg: ((w >> 12) & 0xf) as u8,
            plat: ((w >> 16) & 0xf) as u8,
            cut: ((w >> 24) & 0xf) as u8,
        }
    }
}

/// Upstream `check_positive` for chips other than the 8812A/8821A: a zero
/// cut, package or interface in the header matches anything; the RFE type
/// must match exactly (0 included). The platform is not compared.
pub fn check_positive(drv: &PhyCond, cond: &PhyCond) -> bool {
    if cond.cut != 0 && cond.cut != drv.cut {
        return false;
    }
    if cond.pkg != 0 && cond.pkg != drv.pkg {
        return false;
    }
    if cond.intf != 0 && cond.intf != drv.intf {
        return false;
    }
    cond.rfe == drv.rfe
}

/// Upstream `rtw_parse_tbl_phy_cond`: walk `words` as pairs and call `cfg`
/// with each write the condition `drv` selects. The first matching branch
/// of a chain wins (`is_skipped`), `else` applies when none did, and
/// writes outside any chain always apply. A trailing odd word is ignored,
/// as upstream's `size / 2` does.
pub fn parse_phy_cond(drv: &PhyCond, words: &[u32], mut cfg: impl FnMut(u32, u32)) {
    let mut pos_cond = PhyCond::default();
    let mut is_matched = true;
    let mut is_skipped = false;
    for pair in words.chunks_exact(2) {
        let (addr, data) = (pair[0], pair[1]);
        if addr & (1 << 31) != 0 {
            match (addr >> 28) & 0x3 {
                BRANCH_ENDIF => {
                    is_matched = true;
                    is_skipped = false;
                }
                BRANCH_ELSE => is_matched = !is_skipped,
                // BRANCH_IF, BRANCH_ELIF: remember the condition for the
                // `neg` word that follows.
                _ => pos_cond = PhyCond::from_word(addr),
            }
        } else if addr & (1 << 30) != 0 {
            if !is_skipped {
                if check_positive(drv, &pos_cond) {
                    is_matched = true;
                    is_skipped = true;
                } else {
                    is_matched = false;
                    is_skipped = false;
                }
            } else {
                is_matched = false;
            }
        } else if is_matched {
            cfg(addr, data);
        }
    }
}

/// Upstream `rtw_phy_cfg_bb`: addresses 0xf9 to 0xfe are delays, not
/// registers (0xfe 50 ms, 0xfd 5 ms, 0xfc 1 ms, 0xfb 50 us, 0xfa 5 us,
/// 0xf9 1 us).
pub fn cfg_bb<H: Hci>(hci: &mut H, addr: u32, data: u32) {
    match addr {
        0xfe => hci.delay_us(50_000),
        0xfd => hci.delay_us(5_000),
        0xfc => hci.delay_us(1_000),
        0xfb => hci.delay_us(50),
        0xfa => hci.delay_us(5),
        0xf9 => hci.delay_us(1),
        _ => hci.write32(addr, data),
    }
}

/// Upstream `rtw_phy_cfg_rf`: 0xffe is a 50 ms delay and 0xfe a 100 us one;
/// every other entry writes the whole RF register and waits 1 us.
pub fn cfg_rf<H: Hci>(dev: &mut Dev<H>, path: usize, addr: u32, data: u32) {
    match addr {
        0xffe => dev.hci.delay_us(50_000),
        0xfe => dev.hci.delay_us(100),
        _ => {
            write_rf(dev, path, addr, RFREG_MASK, data);
            dev.hci.delay_us(1);
        }
    }
}

/// Load a MAC table (`rtw_phy_cfg_mac`: byte writes).
pub fn load_mac<H: Hci>(dev: &mut Dev<H>, words: &[u32]) {
    let cond = dev.phy.cond;
    parse_phy_cond(&cond, words, |a, d| dev.hci.write8(a, d as u8));
}

/// Load an AGC table (`rtw_phy_cfg_agc`: plain 32-bit writes).
pub fn load_agc<H: Hci>(dev: &mut Dev<H>, words: &[u32]) {
    let cond = dev.phy.cond;
    parse_phy_cond(&cond, words, |a, d| dev.hci.write32(a, d));
}

/// Load a BB table (`rtw_phy_cfg_bb`, with its delay addresses).
pub fn load_bb<H: Hci>(dev: &mut Dev<H>, words: &[u32]) {
    let cond = dev.phy.cond;
    parse_phy_cond(&cond, words, |a, d| cfg_bb(&mut dev.hci, a, d));
}

/// Load an RF table into `path` (`rtw_phy_cfg_rf`).
pub fn load_rf<H: Hci>(dev: &mut Dev<H>, path: usize, words: &[u32]) {
    let cond = dev.phy.cond;
    parse_phy_cond(&cond, words, |a, d| cfg_rf(dev, path, a, d));
}

/// Upstream `rtw_load_rfk_table`: open the IQK/DPK clocks, load the
/// calibration init table as a BB table, and mark DPK as powered.
pub fn load_rfk_table<H: Hci>(dev: &mut Dev<H>) {
    dev.hci.write32_mask(0x1e24, 1 << 17, 0x1);
    dev.hci.write32_mask(0x1cd0, 1 << 28, 0x1);
    dev.hci.write32_mask(0x1cd0, 1 << 29, 0x1);
    dev.hci.write32_mask(0x1cd0, 1 << 30, 0x1);
    dev.hci.write32_mask(0x1cd0, 1 << 31, 0x0);
    load_bb(dev, tables::CAL_INIT);
    dev.dm.dpk.is_dpk_pwr_on = true;
}

/// Upstream `rtw_phy_load_tables`: MAC, BB, AGC (the 8822C has no
/// `agc_btg` table), the RF calibration init table, then one RF table per
/// path in upstream's `rf_tbl[]` order ([`tables::RF_TABLES`]).
pub fn load_tables<H: Hci>(dev: &mut Dev<H>) {
    load_mac(dev, tables::MAC);
    load_bb(dev, tables::BB);
    load_agc(dev, tables::AGC);
    load_rfk_table(dev);
    let paths = usize::from(dev.hal.rf_path_num).min(tables::RF_TABLES.len());
    for &(path, words) in &tables::RF_TABLES[..paths] {
        load_rf(dev, path, words);
    }
}
