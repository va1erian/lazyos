//! The TX power limit table (upstream `phy.c`: `rtw_channel_to_idx`,
//! `rtw_phy_set_tx_power_limit`, `rtw_parse_tbl_txpwr_lmt`,
//! `rtw_xref_txpwr_lmt` and its helpers, `rtw_phy_tx_power_limit_config`),
//! for the world-wide set only (see [`crate::phy::PhyState`]).

use crate::phy::rate::*;
use crate::phy::{PhyState, BW_MAX, MAX_CHANNEL_NUM_5G, MAX_POWER_INDEX, REGD_WW};

/// Upstream `RTW_REGD_MAX`.
const REGD_MAX: u8 = 13;

/// Upstream `rtw_channel_idx_5g`.
const CHANNEL_IDX_5G: [u8; MAX_CHANNEL_NUM_5G] = [
    36, 38, 40, 42, 44, 46, 48, // band 1
    52, 54, 56, 58, 60, 62, 64, // band 2
    100, 102, 104, 106, 108, 110, 112, // band 3
    116, 118, 120, 122, 124, 126, 128, // band 3
    132, 134, 136, 138, 140, 142, 144, // band 3
    149, 151, 153, 155, 157, 159, 161, // band 4
    165, 167, 169, 171, 173, 175, 177, // band 4
];

/// Upstream `rtw_channel_to_idx`: the column of `channel` in the 2.4 GHz
/// (`band_5g == false`) or 5 GHz limit arrays.
pub fn channel_to_idx(band_5g: bool, channel: u8) -> Option<usize> {
    if band_5g {
        CHANNEL_IDX_5G.iter().position(|&c| c == channel)
    } else {
        let idx = usize::from(channel).checked_sub(1)?;
        (idx < super::MAX_CHANNEL_NUM_2G).then_some(idx)
    }
}

/// Upstream `rtw_phy_set_tx_power_limit`, the world-wide column: a WW row
/// sets the limit, any other domain's row lowers it (so WW ends as the
/// minimum over every domain, unless a later WW row overrides it, exactly
/// as upstream's order gives).
fn set_limit(phy: &mut PhyState, regd: u8, band: u8, bw: u8, rs: u8, ch: u8, lmt: i8) {
    let max = MAX_POWER_INDEX as i8;
    let lmt = lmt.clamp(-max, max);
    let Some(ch_idx) = channel_to_idx(band == 1, ch) else {
        return;
    };
    if regd >= REGD_MAX || usize::from(bw) >= BW_MAX || usize::from(rs) >= RATE_SECTION_NUM {
        return;
    }
    let cell = match band {
        0 => &mut phy.limit_2g[usize::from(bw)][usize::from(rs)][ch_idx],
        1 => &mut phy.limit_5g[usize::from(bw)][usize::from(rs)][ch_idx],
        _ => return,
    };
    *cell = if regd == REGD_WW {
        lmt
    } else {
        (*cell).min(lmt)
    };
}

/// Upstream `rtw_parse_tbl_txpwr_lmt`, keeping the world-wide limits, then
/// `rtw_xref_txpwr_lmt` on them: in 5 GHz, at 20 and 40 MHz, a HT or VHT
/// section left at the maximum takes the other's limit. Rows are
/// `[regd, band, bw, rate section, channel, limit as i8]`.
pub fn parse_txpwr_lmt(phy: &mut PhyState, rows: &[[u8; 6]]) {
    for &[regd, band, bw, rs, ch, lmt] in rows {
        set_limit(phy, regd, band, bw, rs, ch, lmt as i8);
    }
    let max = MAX_POWER_INDEX as i8;
    let pairs = [
        (RS_HT_1S, RS_VHT_1S),
        (RS_HT_2S, RS_VHT_2S),
        (RS_HT_3S, RS_VHT_3S),
        (RS_HT_4S, RS_VHT_4S),
    ];
    // 20 and 40 MHz only.
    for t in phy.limit_5g.iter_mut().take(2) {
        for ch in 0..MAX_CHANNEL_NUM_5G {
            for &(ht, vht) in &pairs {
                let (l_ht, l_vht) = (t[ht][ch], t[vht][ch]);
                if l_ht == l_vht {
                    continue;
                }
                if l_ht == max {
                    t[ht][ch] = l_vht;
                } else if l_vht == max {
                    t[vht][ch] = l_ht;
                }
            }
        }
    }
}

/// Upstream `rtw_phy_tx_power_limit_config`: make each limit relative to
/// its rate section's by-rate base (path A's, as upstream).
pub fn limit_config(phy: &mut PhyState) {
    for bw in 0..BW_MAX {
        for rs in 0..RATE_SECTION_NUM {
            let b2 = phy.by_rate_base_2g[0][rs];
            let b5 = phy.by_rate_base_5g[0][rs];
            for l in phy.limit_2g[bw][rs].iter_mut() {
                *l = l.wrapping_sub(b2);
            }
            for l in phy.limit_5g[bw][rs].iter_mut() {
                *l = l.wrapping_sub(b5);
            }
        }
    }
}
