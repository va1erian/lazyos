//! DPK gain search and the steps after it (upstream `rtw8822c.c`:
//! `rtw8822c_dpk_gainloss`, `_rf_setting`, `_rxbb_dc_cal`,
//! `_dc_corr_check`, `_thermal_read`, `_pas_read`, `rtw8822c_psd_log2base`,
//! `_gainloss_result`, `_agc_gain_chk`, `_agc_loss_chk`, the `dpk_state[]`
//! machine `_pas_agc`, `_cal_gs`, `_cal_coef1`, `_dpk_on`, `_reload`,
//! `_reload_data`).

use crate::dev::{Dev, RF_PATH_A};
use crate::hci::{Hci, RegExt};
use crate::phy::rf::{read_rf, write_rf, RFREG_MASK};
use crate::rtw8822c::dpk::{coef_write, one_shot, Action, DPK_BAND_2G, DPK_BW_80};
use crate::rtw8822c::regs::*;
use crate::rtw8822c::util::check_hw_ready;

fn subpage(path: usize) -> u32 {
    0x8 | ((path as u32) << 1)
}

/// Upstream `rtw8822c_dpk_rxbb_dc_cal`.
pub(super) fn rxbb_dc_cal<H: Hci>(dev: &mut Dev<H>, path: usize) {
    write_rf(dev, path, 0x92, RFREG_MASK, 0x84800);
    dev.hci.delay_us(5);
    write_rf(dev, path, 0x92, RFREG_MASK, 0x84801);
    dev.hci.delay_us(600);
    write_rf(dev, path, 0x92, RFREG_MASK, 0x84800);
}

/// Upstream `rtw8822c_dpk_dc_corr_check`: 1 if the RX DC or correlation
/// is out of range.
fn dc_corr_check<H: Hci>(h: &mut H) -> u8 {
    h.write32(REG_RXSRAM_CTL, 0x0009_00f0);
    let mut dc_i = h.read32_mask(REG_STAT_RPT, genmask(27, 16)) as u16;
    let mut dc_q = h.read32_mask(REG_STAT_RPT, genmask(11, 0)) as u16;
    if dc_i & (1 << 11) != 0 {
        dc_i = 0x1000 - dc_i;
    }
    if dc_q & (1 << 11) != 0 {
        dc_q = 0x1000 - dc_q;
    }
    h.write32(REG_RXSRAM_CTL, 0x0000_00f0);
    let corr_idx = h.read32_mask(REG_STAT_RPT, genmask(7, 0)) as u8;
    let _ = h.read32_mask(REG_STAT_RPT, genmask(15, 8));
    u8::from(dc_i > 200 || dc_q > 200 || !(40..=65).contains(&corr_idx))
}

/// Upstream `rtw8822c_dpk_thermal_read`.
pub(super) fn thermal_read<H: Hci>(dev: &mut Dev<H>, path: usize) -> u8 {
    write_rf(dev, path, RF_T_METER, bit(19), 0x1);
    write_rf(dev, path, RF_T_METER, bit(19), 0x0);
    write_rf(dev, path, RF_T_METER, bit(19), 0x1);
    dev.hci.delay_us(15);
    read_rf(dev, path, RF_T_METER, 0x0007e) as u8
}

/// Upstream `rtw8822c_dpk_pas_read`: power of the PA output sample.
fn pas_read<H: Hci>(h: &mut H, path: usize) -> u32 {
    h.write32(REG_NCTL0, subpage(path));
    h.write32_mask(0x1b48, bit(14), 0x0);
    h.write32(REG_RXSRAM_CTL, 0x0006_0001);
    h.write32(0x1b4c, 0x0000_0000);
    h.write32(0x1b4c, 0x0008_0000);
    let mut q = h.read32_mask(REG_STAT_RPT, MASKHWORD);
    let mut i = h.read32_mask(REG_STAT_RPT, MASKLWORD);
    if i & (1 << 15) != 0 {
        i = 0x10000 - i;
    }
    if q & (1 << 15) != 0 {
        q = 0x10000 - q;
    }
    h.write32(0x1b4c, 0x0000_0000);
    i.wrapping_mul(i).wrapping_add(q.wrapping_mul(q))
}

/// Upstream `rtw8822c_psd_log2base`: 100 x log2, from a fraction table.
pub fn psd_log2base(val: u32) -> u32 {
    const FRACTION: [u32; 21] = [
        0, 432, 332, 274, 232, 200, 174, 151, 132, 115, 100, 86, 74, 62, 51, 42, 32, 23, 15, 7, 0,
    ];
    if val == 0 {
        return 0;
    }
    let int_b = 32 - val.leading_zeros(); // __fls(val) + 1
    let tmp = (u64::from(val) * 100 / (1u64 << int_b)) as u32;
    let tindex = ((tmp / 5) as usize).min(FRACTION.len() - 1);
    (int_b * 100).wrapping_sub(FRACTION[tindex])
}

fn gainloss_result<H: Hci>(h: &mut H, path: usize) -> u8 {
    h.write32_mask(REG_NCTL0, BIT_SUBPAGE, subpage(path));
    h.write32_mask(0x1b48, bit(14), 0x1);
    h.write32(REG_RXSRAM_CTL, 0x0006_0000);
    let r = h.read32_mask(REG_STAT_RPT, 0xf0) as u8;
    h.write32_mask(0x1b48, bit(14), 0x0);
    r
}

fn dgain_read<H: Hci>(h: &mut H) -> u16 {
    h.write32_mask(REG_NCTL0, BIT_SUBPAGE, 0xc);
    h.write32_mask(REG_RXSRAM_CTL, 0x00ff_0000, 0x0);
    h.read32_mask(REG_STAT_RPT, genmask(27, 16)) as u16
}

/// States of upstream's `dpk_state[]` (`enum rtw8822c_dpk_agc_phase`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    GainCheck,
    GainLarge,
    GainLess,
    GlLarge,
    GlLess,
    LossCheck,
    AgcOut,
}

struct AgcData {
    txbb: u8,
    pga: u8,
    limited_pga: bool,
    agc_cnt: u8,
    loss_only: bool,
    gain_only: bool,
    path: usize,
}

fn step<H: Hci>(dev: &mut Dev<H>, d: &mut AgcData, state: State) -> State {
    let path = d.path;
    match state {
        State::GainCheck => {
            d.txbb = read_rf(dev, path, RF_TX_GAIN, BIT_GAIN_TXBB) as u8;
            d.pga = read_rf(dev, path, RF_MODE_TRXAGC, BIT_RXAGC) as u8;
            let mut s = if d.loss_only {
                State::LossCheck
            } else {
                one_shot(dev, path, Action::Dagc);
                let dgain = dgain_read(&mut dev.hci);
                let s = if dgain > 1535 && !d.limited_pga {
                    State::GainLess
                } else if dgain < 768 && !d.limited_pga {
                    State::GainLarge
                } else {
                    State::GainCheck
                };
                match s {
                    State::GainCheck if d.gain_only => State::AgcOut,
                    State::GainCheck => State::LossCheck,
                    other => other,
                }
            };
            d.agc_cnt += 1;
            if d.agc_cnt >= 6 {
                s = State::AgcOut;
            }
            s
        }
        State::GainLarge | State::GainLess => {
            let large = state == State::GainLarge;
            let pga = d.pga;
            let set = |v: u32, dev: &mut Dev<H>| {
                write_rf(dev, path, RF_MODE_TRXAGC, BIT_RXAGC, v);
            };
            if large {
                if pga > 0xe {
                    set(0xc, dev);
                } else if pga > 0xb && pga < 0xf {
                    set(0x0, dev);
                } else if pga < 0xc {
                    d.limited_pga = true;
                }
            } else if pga < 0xc {
                set(0xc, dev);
            } else if pga > 0xb && pga < 0xf {
                set(0xf, dev);
            } else if pga > 0xe {
                d.limited_pga = true;
            }
            State::GainCheck
        }
        State::GlLarge | State::GlLess => {
            let large = state == State::GlLarge;
            let bound = if large { 0 } else { 0x1f };
            if d.txbb == bound {
                return State::AgcOut;
            }
            d.txbb = if large {
                d.txbb.wrapping_sub(2)
            } else {
                d.txbb.wrapping_add(3)
            };
            write_rf(dev, path, RF_TX_GAIN, BIT_GAIN_TXBB, u32::from(d.txbb));
            d.limited_pga = false;
            State::GainCheck
        }
        State::LossCheck => {
            one_shot(dev, path, Action::GainLoss);
            let loss = pas_read(&mut dev.hci, path);
            if loss < 0x400_0000 {
                return State::GlLess;
            }
            let loss_db = (3 * psd_log2base(loss >> 13)).wrapping_sub(3870);
            if loss_db > 1000 {
                State::GlLarge
            } else if loss_db < 250 {
                State::GlLess
            } else {
                State::AgcOut
            }
        }
        State::AgcOut => State::AgcOut,
    }
}

/// Upstream `rtw8822c_dpk_pas_agc`: the TX baseband gain the search ends at.
fn pas_agc<H: Hci>(dev: &mut Dev<H>, path: usize, gain_only: bool, loss_only: bool) -> u8 {
    let mut d = AgcData {
        txbb: 0,
        pga: 0,
        limited_pga: false,
        agc_cnt: 0,
        loss_only,
        gain_only,
        path,
    };
    let mut state = State::GainCheck;
    while state != State::AgcOut {
        state = step(dev, &mut d, state);
    }
    d.txbb
}

/// Upstream `rtw8822c_dpk_rf_setting`: loop the TX back into RX at a
/// known gain; returns the original TX baseband gain.
fn rf_setting<H: Hci>(dev: &mut Dev<H>, path: usize) -> u32 {
    write_rf(dev, path, RF_MODE_TRXAGC, RFREG_MASK, 0x50017);
    let ori_txbb = read_rf(dev, path, RF_TX_GAIN, RFREG_MASK);
    write_rf(dev, path, RF_DEBUG, BIT_DE_TX_GAIN, 0x1);
    write_rf(dev, path, RF_DEBUG, BIT_DE_PWR_TRIM, 0x1);
    write_rf(dev, path, RF_TX_GAIN_OFFSET, BIT_BB_GAIN, 0x0);
    write_rf(dev, path, RF_TX_GAIN, RFREG_MASK, ori_txbb);
    if dev.dm.dpk.dpk_band == DPK_BAND_2G {
        write_rf(dev, path, RF_TX_GAIN_OFFSET, BIT_RF_GAIN, 0x1);
        write_rf(dev, path, RF_RXG_GAIN, BIT_RXG_GAIN, 0x0);
    } else {
        write_rf(dev, path, RF_TXA_LB_SW, BIT_TXA_LB_ATT, 0x0);
        write_rf(dev, path, RF_TXA_LB_SW, BIT_LB_ATT, 0x6);
        write_rf(dev, path, RF_TXA_LB_SW, BIT_LB_SW, 0x1);
        write_rf(dev, path, RF_RXA_MIX_GAIN, BIT_RXA_MIX_GAIN, 0);
    }
    write_rf(dev, path, RF_MODE_TRXAGC, BIT_RXAGC, 0xf);
    write_rf(dev, path, RF_DEBUG, BIT_DE_TRXBW, 0x1);
    write_rf(dev, path, RF_BW_TRXBB, BIT_BW_RXBB, 0x0);
    let txbb_bw = if dev.dm.dpk.dpk_bw == DPK_BW_80 {
        0x2
    } else {
        0x1
    };
    write_rf(dev, path, RF_BW_TRXBB, BIT_BW_TXBB, txbb_bw);
    write_rf(dev, path, RF_EXT_TIA_BW, bit(1), 0x1);
    dev.hci.delay_us(100);
    ori_txbb & 0x1f
}

/// Upstream `rtw8822c_dpk_gainloss`: find the TX AGC at which the PA's
/// gain loss is in range; also records the thermal drift during the search.
pub(super) fn gainloss<H: Hci>(dev: &mut Dev<H>, path: usize) -> u32 {
    let ori_txbb = rf_setting(dev, path) as u8;
    let ori_txagc = read_rf(dev, path, RF_MODE_TRXAGC, BIT_TXAGC) as u8;
    rxbb_dc_cal(dev, path);
    one_shot(dev, path, Action::Dagc);
    dgain_read(&mut dev.hci);
    if dc_corr_check(&mut dev.hci) != 0 {
        rxbb_dc_cal(dev, path);
        one_shot(dev, path, Action::Dagc);
        dc_corr_check(&mut dev.hci);
    }
    let t1 = thermal_read(dev, path);
    let mut tx_bb = pas_agc(dev, path, false, true);
    let search = gainloss_result(&mut dev.hci, path);
    tx_bb = tx_bb.saturating_sub(search);
    write_rf(dev, path, RF_TX_GAIN, BIT_GAIN_TXBB, u32::from(tx_bb));
    let tx_agc = ori_txagc.wrapping_sub(ori_txbb.wrapping_sub(tx_bb));
    let t2 = thermal_read(dev, path);
    dev.dm.dpk.thermal_dpk_delta[path] = (i16::from(t2) - i16::from(t1)).unsigned_abs() as u8;
    u32::from(tx_agc)
}

/// Upstream `rtw8822c_dpk_cal_gs`: the gain scaling for a calibrated path.
fn cal_gs<H: Hci>(dev: &mut Dev<H>, path: usize) {
    let bw80 = dev.dm.dpk.dpk_bw == DPK_BW_80;
    let (ctl0, ctl1) = if path == RF_PATH_A {
        (REG_DPD_CTL0_S0, REG_DPD_CTL1_S0)
    } else {
        (REG_DPD_CTL0_S1, REG_DPD_CTL1_S1)
    };
    let h = &mut dev.hci;
    h.write32_mask(REG_NCTL0, BIT_SUBPAGE, subpage(path));
    h.write32_mask(REG_IQK_CTL1, BIT_BYPASS_DPD, 0x0);
    h.write32_mask(REG_IQK_CTL1, BIT_TX_CFIR, 0x0);
    h.write32_mask(REG_R_CONFIG, BIT_IQ_SWITCH, 0x9);
    h.write32_mask(REG_R_CONFIG, BIT_INNER_LB, 0x1);
    h.write32_mask(REG_NCTL0, BIT_SUBPAGE, 0xc);
    h.write32_mask(REG_RXSRAM_CTL, BIT_DPD_CLK, 0xf);
    h.write32_mask(ctl0, BIT_GS_PWSF, 0x106_6680);
    h.write32_mask(ctl1, BIT_DPD_EN, 0x1);
    let seq: [u32; 5] = if bw80 {
        [
            0x8000_1310,
            0x0000_1310,
            0x8100_00db,
            0x0100_00db,
            0x0000_b428,
        ]
    } else {
        [
            0x8200_190c,
            0x0200_190c,
            0x8301_ee14,
            0x0301_ee14,
            0x0000_b428,
        ]
    };
    for v in seq {
        h.write32(REG_DPD_CTL16, v);
    }
    let base = if bw80 { 0x0502_0000 } else { 0x0502_0008 };
    h.write32(REG_DPD_CTL15, base | (bit(path as u32) << 28));
    h.write32_mask(REG_DPD_CTL0, MASKBYTE3, 0x8 | path as u32);
    one_shot(dev, path, Action::CalPwr);
    let h = &mut dev.hci;
    h.write32_mask(REG_DPD_CTL15, MASKBYTE3, 0x0);
    h.write32_mask(REG_NCTL0, BIT_SUBPAGE, subpage(path));
    h.write32_mask(REG_R_CONFIG, BIT_IQ_SWITCH, 0x0);
    h.write32_mask(REG_R_CONFIG, BIT_INNER_LB, 0x0);
    h.write32_mask(REG_NCTL0, BIT_SUBPAGE, 0xc);
    h.write32_mask(ctl0, BIT_GS_PWSF, 0x5b);
    h.write32_mask(REG_RXSRAM_CTL, BIT_RPT_SEL, 0x0);
    let gs = h.read32_mask(REG_STAT_RPT, BIT_RPT_DGAIN) as u16 as u32;
    let gs = (gs * 910) >> 10;
    let gs = (gs + 5) / 10; // DIV_ROUND_CLOSEST(gs, 10)
    h.write32_mask(ctl0, BIT_GS_PWSF, gs);
    dev.dm.dpk.dpk_gs[path] = gs as u16;
}

/// Upstream `rtw8822c_dpk_on`.
pub(super) fn dpk_on<H: Hci>(dev: &mut Dev<H>, path: usize) {
    one_shot(dev, path, Action::DpkOn);
    dev.hci.write32_mask(REG_NCTL0, BIT_SUBPAGE, subpage(path));
    dev.hci.write32_mask(REG_IQK_CTL1, BIT_TX_CFIR, 0x0);
    if dev.dm.dpk.path_ok & (1 << path) != 0 {
        cal_gs(dev, path);
    }
}

/// Upstream `rtw8822c_dpk_cal_coef1`: the I scaling from each path's gain.
pub(super) fn cal_coef1<H: Hci>(dev: &mut Dev<H>) {
    let h = &mut dev.hci;
    h.write32_mask(REG_NCTL0, BIT_SUBPAGE, 0xc);
    h.write32(REG_RXSRAM_CTL, 0x0000_00f0);
    h.write32(REG_NCTL0, 0x0000_1148);
    h.write32(REG_NCTL0, 0x0000_1149);
    // Upstream warns "DPK stuck, performance may be suboptimal".
    let _ = check_hw_ready(h, 0x2d9c, MASKBYTE0, 0x55);
    h.write8(0x1b10, 0x0);
    h.write32_mask(REG_NCTL0, BIT_SUBPAGE, 0xc);
    for path in 0..usize::from(dev.hal.rf_path_num).min(2) {
        let off = [0u32, 0x58][path];
        // dpk_gs is never 0 here (result_reset sets 0x5b); guard anyway.
        let i_scaling = 0x16c00 / u32::from(dev.dm.dpk.dpk_gs[path]).max(1);
        let h = &mut dev.hci;
        h.write32_mask(0x1b18 + off, MASKHWORD, i_scaling);
        h.write32_mask(REG_DPD_CTL0_S0 + off, genmask(31, 28), 0x9);
        h.write32_mask(REG_DPD_CTL0_S0 + off, genmask(31, 28), 0x1);
        h.write32_mask(REG_DPD_CTL0_S0 + off, genmask(31, 28), 0x0);
        h.write32_mask(REG_DPD_CTL1_S0 + off, bit(14), 0x0);
    }
}

/// Upstream `rtw8822c_dpk_reload` and `_reload_data`: on the channel the
/// last DPK ran on, put its results back instead of measuring.
pub(super) fn reload<H: Hci>(dev: &mut Dev<H>) -> bool {
    dev.dm.dpk.is_reload = false;
    let channel = (read_rf(dev, RF_PATH_A, 0x18, RFREG_MASK) & 0xff) as u8;
    if channel != dev.dm.dpk.dpk_ch {
        return false;
    }
    let d = dev.dm.dpk.clone();
    if d.path_ok & 0x3 != 0 || d.dpk_ch != 0 {
        for path in 0..usize::from(dev.hal.rf_path_num).min(2) {
            let h = &mut dev.hci;
            h.write32_mask(REG_NCTL0, BIT_SUBPAGE, subpage(path));
            h.write32(
                REG_DPD_CTL1_S1,
                if d.dpk_band == DPK_BAND_2G {
                    0x1f10_0000
                } else {
                    0x1f0d_0000
                },
            );
            h.write8(REG_DPD_AGC, d.dpk_txagc[path]);
            coef_write(dev, path, d.path_ok & (1 << path) != 0);
            one_shot(dev, path, Action::DpkOn);
            let ctl0 = if path == RF_PATH_A {
                REG_DPD_CTL0_S0
            } else {
                REG_DPD_CTL0_S1
            };
            dev.hci.write32_mask(REG_NCTL0, BIT_SUBPAGE, 0xc);
            dev.hci
                .write32_mask(ctl0, BIT_GS_PWSF, u32::from(d.dpk_gs[path]));
        }
        cal_coef1(dev);
    }
    dev.dm.dpk.is_reload = true;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log2base_matches_upstream_table() {
        assert_eq!(psd_log2base(0), 0);
        // 1: __fls = 0 -> int 1, tmp = 100 / 2 = 50, index 10 -> 100 - 100 = 0
        assert_eq!(psd_log2base(1), 0);
        // 0x8000: int 16, tmp = 50 -> 1600 - 100
        assert_eq!(psd_log2base(0x8000), 1500);
        // 0xffff: int 16, tmp = 99 -> index 19 -> 1600 - 7
        assert_eq!(psd_log2base(0xffff), 1593);
    }
}
