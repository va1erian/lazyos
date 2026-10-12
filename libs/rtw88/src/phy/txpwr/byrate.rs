//! The TX power by-rate table (upstream `phy.c`:
//! `rtw_phy_get_rate_values_of_txpwr_by_rate`,
//! `rtw_phy_store_tx_power_by_rate`, `rtw_parse_tbl_bb_pg`,
//! `rtw_phy_tx_power_by_rate_config`).
//!
//! Each row names a legacy power register (`0xc20`, `0xe24`, ...) whose four
//! bytes are the power of four rates; which rates is fixed by the register.

use crate::dev::{Dev, RF_PATH_MAX};
use crate::hci::Hci;
use crate::phy::rate::*;
use crate::phy::PhyState;

const fn rates4(first: u8) -> [u8; 4] {
    [first, first + 1, first + 2, first + 3]
}

/// `(rates, first byte, bcd)` for a register; `bcd` marks the two rows
/// upstream decodes as BCD whatever the chip (`bcd_to_dec_pwr_by_rate`).
fn rate_values(addr: u32, mask: u32) -> Option<(&'static [u8], u32, bool)> {
    const OFDM_LO: [u8; 4] = rates4(DESC_RATE6M);
    const OFDM_HI: [u8; 4] = rates4(DESC_RATE24M);
    const CCK: [u8; 4] = rates4(DESC_RATE1M);
    const MCS0: [u8; 4] = rates4(DESC_RATEMCS0);
    const MCS4: [u8; 4] = rates4(DESC_RATEMCS0 + 4);
    const MCS8: [u8; 4] = rates4(DESC_RATEMCS8);
    const MCS12: [u8; 4] = rates4(DESC_RATEMCS8 + 4);
    const MCS16: [u8; 4] = rates4(DESC_RATEMCS16);
    const MCS20: [u8; 4] = rates4(DESC_RATEMCS16 + 4);
    const VHT1_0: [u8; 4] = rates4(DESC_RATEVHT1SS_MCS0);
    const VHT1_4: [u8; 4] = rates4(DESC_RATEVHT1SS_MCS0 + 4);
    const VHT1_8: [u8; 4] = rates4(DESC_RATEVHT1SS_MCS0 + 8);
    const VHT2_2: [u8; 4] = rates4(DESC_RATEVHT2SS_MCS0 + 2);
    const VHT2_6: [u8; 4] = rates4(DESC_RATEVHT2SS_MCS0 + 6);
    const VHT3_0: [u8; 4] = rates4(DESC_RATEVHT3SS_MCS0);
    const VHT3_4: [u8; 4] = rates4(DESC_RATEVHT3SS_MCS0 + 4);
    const VHT3_8: [u8; 2] = [DESC_RATEVHT3SS_MCS0 + 8, DESC_RATEVHT3SS_MCS0 + 9];
    const ONE: [u8; 1] = [DESC_RATE1M];
    const ELEVEN: [u8; 1] = [DESC_RATE11M];
    const CCK_HI: [u8; 3] = [DESC_RATE2M, DESC_RATE5_5M, DESC_RATE11M];
    const CCK_LO: [u8; 3] = [DESC_RATE1M, DESC_RATE2M, DESC_RATE5_5M];
    let four = |a: u32| [0xc00 | a, 0xe00 | a, 0x1800 | a, 0x1a00 | a];
    Some(match addr {
        0xe00 | 0x830 => (&OFDM_LO, 0, false),
        0xe04 | 0x834 => (&OFDM_HI, 0, false),
        0xe08 => (&ONE, 1, true),
        0x86c if mask == 0xffff_ff00 => (&CCK_HI, 1, false),
        0x86c if mask == 0x0000_00ff => (&ELEVEN, 0, true),
        0xe10 | 0x83c => (&MCS0, 0, false),
        0xe14 | 0x848 => (&MCS4, 0, false),
        0xe18 | 0x84c => (&MCS8, 0, false),
        0xe1c | 0x868 => (&MCS12, 0, false),
        0x838 => (&CCK_LO, 1, false),
        a if four(0x20).contains(&a) => (&CCK, 0, false),
        a if four(0x24).contains(&a) => (&OFDM_LO, 0, false),
        a if four(0x28).contains(&a) => (&OFDM_HI, 0, false),
        a if four(0x2c).contains(&a) => (&MCS0, 0, false),
        a if four(0x30).contains(&a) => (&MCS4, 0, false),
        a if four(0x34).contains(&a) => (&MCS8, 0, false),
        a if four(0x38).contains(&a) => (&MCS12, 0, false),
        a if four(0x3c).contains(&a) => (&VHT1_0, 0, false),
        a if four(0x40).contains(&a) => (&VHT1_4, 0, false),
        // VHT1SS MCS8/9 then VHT2SS MCS0/1: four consecutive codes.
        a if four(0x44).contains(&a) => (&VHT1_8, 0, false),
        a if four(0x48).contains(&a) => (&VHT2_2, 0, false),
        a if four(0x4c).contains(&a) => (&VHT2_6, 0, false),
        a if four(0xd8).contains(&a) => (&MCS16, 0, false),
        a if four(0xdc).contains(&a) => (&MCS20, 0, false),
        a if four(0xe0).contains(&a) => (&VHT3_0, 0, false),
        a if four(0xe4).contains(&a) => (&VHT3_4, 0, false),
        a if four(0xe8).contains(&a) => (&VHT3_8, 0, false),
        _ => return None,
    })
}

fn bcd2bin(v: u8) -> u8 {
    (v & 0x0f) + (v >> 4) * 10
}

/// Upstream `rtw_phy_store_tx_power_by_rate`: one row into the per-rate
/// offsets. Rows for an unknown register, a band past 5 GHz or a path the
/// chip lacks are dropped (upstream warns).
fn store(phy: &mut PhyState, band: u32, path: u32, addr: u32, mask: u32, data: u32) {
    let Some((rates, first, bcd)) = rate_values(addr, mask) else {
        return;
    };
    let path = path as usize;
    if path >= RF_PATH_MAX || band > 1 {
        return;
    }
    for (i, &rate) in rates.iter().enumerate() {
        let byte = (data >> ((first + i as u32) * 8)) as u8;
        // The 8822C is not `is_pwr_by_rate_dec`: bytes are binary except
        // the two rows upstream always reads as BCD.
        let offset = if bcd { bcd2bin(byte) } else { byte } as i8;
        let table = if band == 0 {
            &mut phy.by_rate_2g
        } else {
            &mut phy.by_rate_5g
        };
        table[path][usize::from(rate)] = offset;
    }
}

/// Upstream `rtw_parse_tbl_bb_pg`; rows naming 0xfe/0xffe are 50 ms delays.
pub fn parse_bb_pg<H: Hci>(dev: &mut Dev<H>, rows: &[[u32; 6]]) {
    for &[band, path, _tx_num, addr, mask, data] in rows {
        if addr == 0xfe || addr == 0xffe {
            dev.hci.delay_us(50_000);
            continue;
        }
        store(&mut dev.phy, band, path, addr, mask, data);
    }
}

/// Upstream `rtw_phy_tx_power_by_rate_config`: each rate section's base is
/// its highest rate (the third-highest for VHT, MCS7 of the ten); every
/// offset becomes relative to its section's base.
pub fn by_rate_config(phy: &mut PhyState) {
    for path in 0..RF_PATH_MAX {
        for (rs, rates) in RATE_SECTION.iter().enumerate() {
            let base_idx = usize::from(if rates.len() == 10 {
                rates[rates.len() - 3]
            } else {
                rates[rates.len() - 1]
            });
            let base_2g = phy.by_rate_2g[path][base_idx];
            let base_5g = phy.by_rate_5g[path][base_idx];
            phy.by_rate_base_2g[path][rs] = base_2g;
            phy.by_rate_base_5g[path][rs] = base_5g;
            for &rate in rates.iter() {
                let r = usize::from(rate);
                phy.by_rate_2g[path][r] = phy.by_rate_2g[path][r].wrapping_sub(base_2g);
                phy.by_rate_5g[path][r] = phy.by_rate_5g[path][r].wrapping_sub(base_5g);
            }
        }
    }
}
