//! Programming TX power into the chip (upstream
//! `rtw8822c_set_tx_power_index`, `rtw8822c_set_write_tx_power_ref`,
//! `rtw8822c_set_tx_power_diff`).
//!
//! The 8822C takes a reference index per path for CCK (the 11M index) and
//! for OFDM (MCS7's), and then, for every group of four rate codes, the
//! signed difference from the reference, one byte per rate at
//! `0x3a00 + rate`. Both paths share the difference table, so the smaller
//! of the two paths' differences is written.

use crate::dev::{Dev, RF_PATH_A, RF_PATH_B};
use crate::hci::{Hci, RegExt};
use crate::phy::rate::{DESC_RATE11M, DESC_RATEMCS7, RATE_SECTION, RS_2SS_MAX};
use crate::rtw8822c::regs::{bit, MASKDWORD};

const TXREF_CCK: [u32; 2] = [0x18a0, 0x41a0];
const TXREF_OFDM: [u32; 2] = [0x18e8, 0x41e8];
const OFFSET_TXAGC: u32 = 0x3a00;

/// Upstream `rtw8822c_set_tx_power_index`.
pub fn set_tx_power_index<H: Hci>(dev: &mut Dev<H>) {
    let tbl = dev.phy.tx_pwr_tbl;
    let r11 = usize::from(DESC_RATE11M);
    let r7 = usize::from(DESC_RATEMCS7);
    let ref_cck = [tbl[RF_PATH_A][r11], tbl[RF_PATH_B][r11]];
    let ref_ofdm = [tbl[RF_PATH_A][r7], tbl[RF_PATH_B][r7]];
    let paths = usize::from(dev.hal.rf_path_num).min(2);

    // rtw8822c_set_write_tx_power_ref
    let h = &mut dev.hci;
    for path in 0..paths {
        h.write32_mask(0x1c90, bit(15), 0);
        h.write32_mask(TXREF_CCK[path], 0x7f_0000, u32::from(ref_cck[path]));
    }
    for path in 0..paths {
        h.write32_mask(0x1c90, bit(15), 0);
        h.write32_mask(TXREF_OFDM[path], 0x1_fc00, u32::from(ref_ofdm[path]));
    }

    let mut diff_idx = [0i8; 4];
    for (rs, rates) in RATE_SECTION.iter().enumerate().take(RS_2SS_MAX + 1) {
        let (ra, rb) = if rs == 0 {
            (ref_cck[0], ref_cck[1])
        } else {
            (ref_ofdm[0], ref_ofdm[1])
        };
        for &rate in rates.iter() {
            let r = usize::from(rate);
            let diff_a = (tbl[RF_PATH_A][r] as i8).wrapping_sub(ra as i8);
            let diff_b = (tbl[RF_PATH_B][r] as i8).wrapping_sub(rb as i8);
            diff_idx[r % 4] = diff_a.min(diff_b);
            if rate % 4 == 3 {
                set_tx_power_diff(h, rate - 3, &diff_idx);
            }
        }
    }
}

/// Upstream `rtw8822c_set_tx_power_diff`: four 7-bit differences into the
/// dword for `rate` (a multiple of 4).
fn set_tx_power_diff<H: Hci>(h: &mut H, rate: u8, diff_idx: &[i8; 4]) {
    let word = diff_idx.iter().enumerate().fold(0u32, |w, (i, &d)| {
        w | (u32::from(d as u8 & 0x7f) << (8 * i))
    });
    h.write32_mask(0x1c90, bit(15), 0x0);
    h.write32_mask(OFFSET_TXAGC + u32::from(rate & 0xfc), MASKDWORD, word);
}
