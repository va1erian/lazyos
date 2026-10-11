//! TX power (upstream `phy.c`): the efuse power index tables
//! (`struct rtw_txpwr_idx`), the by-rate table (`rtw_parse_tbl_bb_pg`,
//! `rtw_phy_tx_power_by_rate_config`), the limit table
//! (`rtw_parse_tbl_txpwr_lmt`, `rtw_xref_txpwr_lmt`,
//! `rtw_phy_tx_power_limit_config`) and the per-rate index the chip is
//! programmed with (`rtw_phy_get_tx_power_index`,
//! `rtw_phy_set_tx_power_level`).
//!
//! The index for a rate is the efuse base for the channel group and width,
//! plus the smaller of the by-rate offset and the regulatory limit (both
//! relative to the rate section's base), minus 6 dB for the rates DPD is
//! disabled on. Only the world-wide limits are kept (see
//! [`super::PhyState`]); SAR is not configured, which upstream treats as no
//! SAR limit; the 8822C has no TX AGC remnant.

use super::rate::*;
use super::{is_ch_2g, PhyState, BW_MAX, MAX_CHANNEL_NUM_2G, MAX_CHANNEL_NUM_5G, MAX_POWER_INDEX, REGD_WW, TXGI_FACTOR};
use crate::dev::{Band, Bw, Dev, RF_PATH_MAX};
use crate::hci::Hci;

mod byrate;
mod efuse;
mod limit;

pub use byrate::{by_rate_config, parse_bb_pg};
pub use efuse::TxPwrIdx;
pub use limit::{channel_to_idx, limit_config, parse_txpwr_lmt};

/// Upstream `rtw_phy_init_tx_power`: zero offsets, limits at the maximum.
pub fn init_tx_power(phy: &mut PhyState) {
    let max = MAX_POWER_INDEX as i8;
    phy.by_rate_2g = [[0; DESC_RATE_MAX]; RF_PATH_MAX];
    phy.by_rate_5g = [[0; DESC_RATE_MAX]; RF_PATH_MAX];
    phy.limit_2g = [[[max; MAX_CHANNEL_NUM_2G]; RATE_SECTION_NUM]; BW_MAX];
    phy.limit_5g = [[[max; MAX_CHANNEL_NUM_5G]; RATE_SECTION_NUM]; BW_MAX];
}

/// Upstream `rtw_get_channel_group`: which efuse base a channel uses
/// (unknown channels fall back to group 0, after upstream's `WARN`).
pub fn channel_group(channel: u8, rate: u8) -> usize {
    match channel {
        3..=5 | 44 | 46 | 48 | 50 => 1,
        6..=8 | 52 | 54 | 56 | 58 => 2,
        9..=11 | 60 | 62 | 64 => 3,
        12 | 13 | 100 | 102 | 104 | 106 => 4,
        14 => {
            if rate <= DESC_RATE11M {
                5
            } else {
                4
            }
        }
        108 | 110 | 112 | 114 => 5,
        116 | 118 | 120 | 122 => 6,
        124 | 126 | 128 | 130 => 7,
        132 | 134 | 136 | 138 => 8,
        140 | 142 | 144 => 9,
        149 | 151 | 153 | 155 => 10,
        157 | 159 | 161 => 11,
        165 | 167 | 169 | 171 => 12,
        173 | 175 | 177 => 13,
        _ => 0,
    }
}

struct Streams {
    mcs: bool,
    above_2ss: bool,
    above_3ss: bool,
    above_4ss: bool,
}

fn streams(rate: u8) -> Streams {
    let ht = |lo: u8| rate >= lo && rate <= DESC_RATEMCS31;
    Streams {
        mcs: ht(DESC_RATEMCS0) || (DESC_RATEVHT1SS_MCS0..=DESC_RATEVHT4SS_MCS9).contains(&rate),
        above_2ss: ht(DESC_RATEMCS8) || rate >= DESC_RATEVHT2SS_MCS0,
        above_3ss: ht(DESC_RATEMCS16) || rate >= DESC_RATEVHT3SS_MCS0,
        above_4ss: ht(DESC_RATEMCS24) || rate >= DESC_RATEVHT4SS_MCS0,
    }
}

/// `tx_power += diff * factor` in upstream's u8 arithmetic.
fn add(p: u8, diff: i8) -> u8 {
    p.wrapping_add((i16::from(diff) * i16::from(TXGI_FACTOR)) as u8)
}

/// Upstream `rtw_phy_get_2g_tx_power_index`.
fn base_2g(idx: &TxPwrIdx, bw: Bw, rate: u8, group: usize) -> u8 {
    let mut p = if rate <= DESC_RATE11M {
        idx.cck_base_2g.get(group).copied().unwrap_or(0)
    } else {
        idx.bw40_base_2g.get(group).copied().unwrap_or(0)
    };
    if (DESC_RATE6M..=DESC_RATE54M).contains(&rate) {
        p = add(p, idx.ht_1s_2g.ofdm);
    }
    let s = streams(rate);
    if !s.mcs {
        return p;
    }
    let ns = &idx.ht_ns_2g;
    match bw {
        // Upstream WARNs on 80 MHz in 2.4 GHz and treats it as 20 MHz.
        Bw::Mhz20 | Bw::Mhz80 => {
            p = add(p, idx.ht_1s_2g.bw20);
            for (above, d) in [(s.above_2ss, ns[0]), (s.above_3ss, ns[1]), (s.above_4ss, ns[2])] {
                if above {
                    p = add(p, d.bw20);
                }
            }
        }
        // bw40 is the base power.
        Bw::Mhz40 => {
            for (above, d) in [(s.above_2ss, ns[0]), (s.above_3ss, ns[1]), (s.above_4ss, ns[2])] {
                if above {
                    p = add(p, d.bw40);
                }
            }
        }
    }
    p
}

/// Upstream `rtw_phy_get_5g_tx_power_index`.
fn base_5g(idx: &TxPwrIdx, bw: Bw, rate: u8, group: usize) -> u8 {
    let lower = idx.bw40_base_5g.get(group).copied().unwrap_or(0);
    let mut p = lower;
    let s = streams(rate);
    if !s.mcs {
        return add(p, idx.ht_1s_5g.ofdm);
    }
    let above = [s.above_2ss, s.above_3ss, s.above_4ss];
    match bw {
        Bw::Mhz20 => {
            p = add(p, idx.ht_1s_5g.bw20);
            for (a, d) in above.iter().zip(idx.ht_ns_5g.iter()) {
                if *a {
                    p = add(p, d.bw20);
                }
            }
        }
        Bw::Mhz40 => {
            for (a, d) in above.iter().zip(idx.ht_ns_5g.iter()) {
                if *a {
                    p = add(p, d.bw40);
                }
            }
        }
        Bw::Mhz80 => {
            // The bw80 base is the average of the two bw40 groups it spans
            // (upstream reads `group + 1` unchecked; it exists for every
            // 80 MHz centre channel).
            let upper = idx.bw40_base_5g.get(group + 1).copied().unwrap_or(lower);
            p = ((u16::from(lower) + u16::from(upper)) / 2) as u8;
            p = add(p, idx.vht_ns_5g[0].bw80);
            for (a, d) in above.iter().zip(idx.vht_ns_5g[1..].iter()) {
                if *a {
                    p = add(p, d.bw80);
                }
            }
        }
    }
    p
}

/// Upstream `rtw_phy_get_tx_power_limit` for the kept (world-wide) set:
/// the smallest limit from 20 MHz up to `bw`, each at its own centre
/// channel (`hal->cch_by_bw`).
fn power_limit(phy: &PhyState, cch_by_bw: &[u8; 3], band_5g: bool, bw: Bw, rate: u8, regd: u8) -> i8 {
    let max = MAX_POWER_INDEX as i8;
    if regd > REGD_WW {
        return max;
    }
    let Some(rs) = rate_section(rate) else { return max };
    let mut bw = bw as usize;
    if rs == RS_CCK || rs == RS_OFDM {
        bw = 0;
    }
    if (DESC_RATEMCS0..=DESC_RATEMCS31).contains(&rate) {
        bw = bw.min(Bw::Mhz40 as usize);
    }
    let mut limit = max;
    for (cur_bw, &cur_ch) in cch_by_bw.iter().enumerate().take(bw + 1) {
        let Some(ch_idx) = channel_to_idx(band_5g, cur_ch) else { return max };
        let cur = if usize::from(cur_ch) <= MAX_CHANNEL_NUM_2G {
            phy.limit_2g[cur_bw][rs].get(ch_idx).copied().unwrap_or(max)
        } else {
            phy.limit_5g[cur_bw][rs].get(ch_idx).copied().unwrap_or(max)
        };
        limit = limit.min(cur);
    }
    limit
}

/// Upstream `rtw_phy_get_dis_dpd_by_rate_diff` with the 8822C's
/// `dpd_ratemask = DIS_DPD_RATEALL`: the low rates DPD is off for go 6 dB
/// (12 index steps) lower.
fn dis_dpd_diff(rate: u8) -> i8 {
    match rate {
        DESC_RATE6M | DESC_RATE9M | DESC_RATEMCS0 | DESC_RATEMCS1 | DESC_RATEMCS8 | DESC_RATEMCS9
        | DESC_RATEVHT1SS_MCS0 | DESC_RATEVHT1SS_MCS1 | DESC_RATEVHT2SS_MCS0 | DESC_RATEVHT2SS_MCS1 => {
            -6 * TXGI_FACTOR as i8
        }
        _ => 0,
    }
}

/// Upstream `rtw_phy_get_tx_power_index`. The arithmetic is upstream's u8
/// wrap-around, clamped to the maximum index.
pub fn get_tx_power_index<H: Hci>(dev: &Dev<H>, path: usize, rate: u8, bw: Bw, channel: u8, regd: u8) -> u8 {
    let phy = &dev.phy;
    if path >= RF_PATH_MAX || usize::from(rate) >= DESC_RATE_MAX {
        return MAX_POWER_INDEX;
    }
    let idx = &phy.txpwr_idx[path];
    let group = channel_group(channel, rate);
    let five = !is_ch_2g(channel);
    let (base, offset) = if five {
        (base_5g(idx, bw, rate, group), phy.by_rate_5g[path][usize::from(rate)])
    } else {
        (base_2g(idx, bw, rate, group), phy.by_rate_2g[path][usize::from(rate)])
    };
    let limit = power_limit(phy, &dev.hal.cch_by_bw, five, bw, rate, regd);
    // No SAR configuration: upstream's `rtw_query_sar` gives the maximum.
    let sar = MAX_POWER_INDEX as i8;
    let offset = offset.min(limit).min(sar).wrapping_add(dis_dpd_diff(rate));
    let p = base.wrapping_add(offset as u8);
    p.min(MAX_POWER_INDEX)
}

/// Upstream `rtw_phy_set_tx_power_level`: compute every rate's index on
/// `channel` for each path (CCK only in 2.4 GHz) into
/// [`PhyState::tx_pwr_tbl`], then program the chip
/// (`rtw8822c_set_tx_power_index`).
pub fn set_tx_power_level<H: Hci>(dev: &mut Dev<H>, channel: u8) {
    let bw = dev.hal.current_band_width;
    let first = if dev.hal.current_band_type == Band::Ghz2 { RS_CCK } else { RS_OFDM };
    let paths = usize::from(dev.hal.rf_path_num).min(RF_PATH_MAX);
    for path in 0..paths {
        for rates in &RATE_SECTION[first..] {
            for &rate in rates.iter() {
                let p = get_tx_power_index(dev, path, rate, bw, channel, REGD_WW);
                dev.phy.tx_pwr_tbl[path][usize::from(rate)] = p;
            }
        }
    }
    crate::rtw8822c::set_tx_power_index(dev);
}
