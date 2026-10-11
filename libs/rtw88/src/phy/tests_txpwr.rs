//! TX power: tables, the per-rate index, and what reaches the chip.
//! Limit values come from an independent Python model of upstream's parser
//! over the C tables; the rest is worked out by hand in the comments.

use super::rate::*;
use super::txpwr::*;
use super::{setup_board_info, MAX_POWER_INDEX};
use crate::dev::{Band, Bw, Dev, RF_PATH_A, RF_PATH_B};
use crate::fake::FakeHci;

/// One path's efuse power table, packed as the efuse stores it: CCK bases
/// 0x2d..0x2f, BW40 bases 0x30..0x34, HT1S OFDM +1 / BW20 -1 (0xf1),
/// 5 GHz BW40 bases 0x28, 0x2a, VHT1S BW80 -1 (high nibble of byte 38).
fn raw_idx() -> crate::efuse::TxPwrIdx {
    let mut b = [0u8; 42];
    b[..6].copy_from_slice(&[0x2d, 0x2d, 0x2e, 0x2e, 0x2f, 0x2f]);
    b[6..11].copy_from_slice(&[0x30, 0x31, 0x32, 0x33, 0x34]);
    b[11] = 0xf1;
    b[18] = 0x28;
    b[19] = 0x2a;
    b[38] = 0xf0;
    crate::efuse::TxPwrIdx(b)
}

fn dev(rfe: u8) -> Dev<FakeHci> {
    let mut d = Dev::new(FakeHci::default());
    d.hal.rf_path_num = 2;
    d.hal.rf_phy_num = 2;
    d.efuse.rfe_option = rfe;
    d.efuse.txpwr_idx_table = [raw_idx(); 4];
    setup_board_info(&mut d).unwrap();
    let t = &d.phy.txpwr_idx[0];
    assert_eq!((t.ht_1s_2g.ofdm, t.ht_1s_2g.bw20, t.vht_ns_5g[0].bw80), (1, -1, -1));
    d
}

#[test]
fn channel_groups_and_indices() {
    assert_eq!(channel_group(1, DESC_RATE1M), 0);
    assert_eq!(channel_group(6, DESC_RATE1M), 2);
    assert_eq!(channel_group(14, DESC_RATE11M), 5);
    assert_eq!(channel_group(14, DESC_RATE6M), 4);
    assert_eq!(channel_group(36, DESC_RATE6M), 0);
    assert_eq!(channel_group(149, DESC_RATE6M), 10);
    assert_eq!(channel_group(177, DESC_RATE6M), 13);
    assert_eq!(channel_to_idx(false, 1), Some(0));
    assert_eq!(channel_to_idx(false, 14), Some(13));
    assert_eq!(channel_to_idx(false, 0), None);
    assert_eq!(channel_to_idx(false, 15), None);
    assert_eq!(channel_to_idx(true, 36), Some(0));
    assert_eq!(channel_to_idx(true, 149), Some(35));
    assert_eq!(channel_to_idx(true, 37), None);
}

#[test]
fn unknown_rfe_type_is_refused() {
    let mut d = Dev::new(FakeHci::default());
    d.efuse.rfe_option = 7;
    assert!(setup_board_info(&mut d).is_err());
}

#[test]
fn by_rate_offsets_are_relative_to_the_section_base() {
    let d = dev(1);
    let p = &d.phy;
    // 0xc20 = 0x484c5054: 1M 0x54, 2M 0x50, 5.5M 0x4c, 11M 0x48 (the base).
    assert_eq!(p.by_rate_base_2g[RF_PATH_A][RS_CCK], 0x48);
    let cck: [i8; 4] = core::array::from_fn(|i| p.by_rate_2g[RF_PATH_A][i]);
    assert_eq!(cck, [0x0c, 0x08, 0x04, 0]);
    // OFDM base is 54M (0x44); 6M was 0x58.
    assert_eq!(p.by_rate_base_2g[RF_PATH_A][RS_OFDM], 0x44);
    assert_eq!(p.by_rate_2g[RF_PATH_A][usize::from(DESC_RATE6M)], 0x14);
    // VHT's base is MCS7 (0x40): MCS8 0x3c, MCS9 0x38 go negative.
    assert_eq!(p.by_rate_base_5g[RF_PATH_B][RS_VHT_1S], 0x40);
    assert_eq!(p.by_rate_5g[RF_PATH_B][usize::from(DESC_RATEVHT1SS_MCS0 + 8)], -4);
    assert_eq!(p.by_rate_5g[RF_PATH_B][usize::from(DESC_RATEVHT1SS_MCS9)], -8);
}

#[test]
fn world_wide_limits_match_the_reference_model() {
    // (rfe, 2.4 GHz [bw][rs][ch], 5 GHz [bw][rs][ch], expected before base)
    for (rfe, lim2, lim5) in [
        (1, [(0, RS_CCK, 0, 60), (0, RS_OFDM, 0, 52), (0, RS_HT_1S, 5, 60), (0, RS_OFDM, 12, 28), (0, RS_CCK, 13, 68), (1, RS_HT_2S, 10, 16)],
            [(0, RS_OFDM, 0, 54), (0, RS_VHT_1S, 0, 54), (1, RS_HT_1S, 1, 54), (2, RS_VHT_1S, 3, 54), (2, RS_VHT_2S, 38, 4), (0, RS_OFDM, 35, 28)]),
        (5, [(0, RS_CCK, 0, 56), (0, RS_OFDM, 0, 52), (0, RS_HT_1S, 5, 60), (0, RS_OFDM, 12, 28), (0, RS_CCK, 13, 72), (1, RS_HT_2S, 10, 26)],
            [(0, RS_OFDM, 0, 58), (0, RS_VHT_1S, 0, 58), (1, RS_HT_1S, 1, 64), (2, RS_VHT_1S, 3, 60), (2, RS_VHT_2S, 38, 4), (0, RS_OFDM, 35, 28)]),
    ] {
        let d = dev(rfe);
        let p = &d.phy;
        for (bw, rs, ch, want) in lim2 {
            let base = p.by_rate_base_2g[0][rs];
            assert_eq!(p.limit_2g[bw][rs][ch], (want as i8).wrapping_sub(base), "rfe {rfe} 2g {bw} {rs} {ch}");
        }
        for (bw, rs, ch, want) in lim5 {
            let base = p.by_rate_base_5g[0][rs];
            assert_eq!(p.limit_5g[bw][rs][ch], (want as i8).wrapping_sub(base), "rfe {rfe} 5g {bw} {rs} {ch}");
        }
    }
}

#[test]
fn power_index_by_hand() {
    let mut d = dev(1);
    d.hal.cch_by_bw = [1, 0, 0];
    // 11M ch1: CCK base 0x2d; offset 0, limit 60 - 72 = -12 -> 0x21.
    assert_eq!(get_tx_power_index(&d, RF_PATH_A, DESC_RATE11M, Bw::Mhz20, 1, 12), 0x21);
    // 1M: offset 0x0c still loses to the limit.
    assert_eq!(get_tx_power_index(&d, RF_PATH_A, DESC_RATE1M, Bw::Mhz20, 1, 12), 0x21);
    // 6M ch1: 0x30 + 1 * 2 = 0x32; min(0x14, 52 - 68) = -16; DPD off -12 -> 0x16.
    assert_eq!(get_tx_power_index(&d, RF_PATH_A, DESC_RATE6M, Bw::Mhz20, 1, 12), 0x16);
    // 54M ch1: 0x32 - 16 = 0x22.
    assert_eq!(get_tx_power_index(&d, RF_PATH_A, DESC_RATE54M, Bw::Mhz20, 1, 12), 0x22);
    // ch6 (group 2): MCS7 0x32 - 2 = 0x30, limit 60 - 64 = -4 -> 0x2c;
    // MCS0 then also loses 12 for DPD -> 0x20.
    d.hal.cch_by_bw = [6, 0, 0];
    assert_eq!(get_tx_power_index(&d, RF_PATH_A, DESC_RATEMCS7, Bw::Mhz20, 6, 12), 0x2c);
    assert_eq!(get_tx_power_index(&d, RF_PATH_A, DESC_RATEMCS0, Bw::Mhz20, 6, 12), 0x20);
    // ch14 CCK uses group 5: 0x2f, limit 68 - 72 = -4 -> 0x2b.
    d.hal.cch_by_bw = [14, 0, 0];
    assert_eq!(get_tx_power_index(&d, RF_PATH_B, DESC_RATE11M, Bw::Mhz20, 14, 12), 0x2b);
    // ch36 54M: 0x28, limit 54 - 68 = -14 -> 0x1a.
    d.hal.cch_by_bw = [36, 38, 42];
    assert_eq!(get_tx_power_index(&d, RF_PATH_A, DESC_RATE54M, Bw::Mhz20, 36, 12), 0x1a);
    // ch42 VHT1SS MCS0 at 80 MHz: (0x28 + 0x2a) / 2 - 2 = 0x27; limits 54
    // at 36/38/42 minus base 0x40 = -10; DPD -12 -> 0x11.
    assert_eq!(get_tx_power_index(&d, RF_PATH_A, DESC_RATEVHT1SS_MCS0, Bw::Mhz80, 42, 12), 0x11);
    // A domain past WW is unlimited; out-of-range inputs give the maximum.
    assert_eq!(get_tx_power_index(&d, 5, DESC_RATE1M, Bw::Mhz20, 36, 12), MAX_POWER_INDEX);
    assert_eq!(get_tx_power_index(&d, RF_PATH_A, 0x60, Bw::Mhz20, 36, 12), MAX_POWER_INDEX);
}

#[test]
fn set_tx_power_level_programs_references_and_differences() {
    let mut d = dev(1);
    d.hal.cch_by_bw = [1, 0, 0];
    d.hal.current_band_type = Band::Ghz2;
    d.hal.current_band_width = Bw::Mhz20;
    d.hci.log.clear();
    set_tx_power_level(&mut d, 1);
    let t = d.phy.tx_pwr_tbl;
    assert_eq!(t[RF_PATH_A][usize::from(DESC_RATE11M)], 0x21);
    let ref_ofdm = t[RF_PATH_A][usize::from(DESC_RATEMCS7)];
    // References: CCK in 0x18a0[22:16], OFDM in 0x18e8[16:10], per path.
    assert_eq!((d.hci.peek32(0x18a0) >> 16) & 0x7f, 0x21);
    assert_eq!((d.hci.peek32(0x41a0) >> 16) & 0x7f, 0x21);
    assert_eq!((d.hci.peek32(0x18e8) >> 10) & 0x7f, u32::from(ref_ofdm));
    // CCK differences: every CCK rate sits at the reference (limit-bound).
    assert_eq!(d.hci.peek32(0x3a00), 0);
    // 6M..18M: (0x16, 0x16, 0x1e, 0x1e) - ref, as 7-bit two's complement.
    let want: [u8; 4] = core::array::from_fn(|i| (t[0][4 + i] as i8).wrapping_sub(ref_ofdm as i8) as u8 & 0x7f);
    assert_eq!(d.hci.peek32(0x3a04).to_le_bytes(), want);
    // Every difference dword of the 1SS/2SS sections was written (CCK to
    // MCS15, VHT1SS MCS0 to VHT2SS MCS9); the 3SS/4SS ones were not.
    assert!(!d.hci.writes().iter().any(|&(_, a, _)| a == 0x3a1c));
    for rate in (0..0x1cu32).step_by(4).chain((0x2c..0x40).step_by(4)) {
        assert!(d.hci.writes().iter().any(|&(_, a, _)| a == 0x3a00 + rate), "0x{:x}", 0x3a00 + rate);
    }
}
