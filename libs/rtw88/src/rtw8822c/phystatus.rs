//! The PHY status (driver info) the chip puts after an RX descriptor
//! (upstream `query_phy_status`, `query_phy_status_page0/1` in
//! `rtw8822c.c`, the `GET_PHY_STAT_P0/P1_*` accessors in `rtw8822c.h`).
//!
//! Page 0 comes with CCK frames, page 1 with OFDM/HT/VHT ones; the page
//! number is the low nibble of byte 0. All fields are little-endian
//! dwords; the offsets below are byte offsets.
//!
//! **Deviation from the contract `query_phy_status(phy_status, pkt)`:**
//! page 0's power needs the CCK gain bounds the chip reports after
//! `phy_set_param`, and page 1's bandwidth falls back to the current width,
//! so the parser takes a small [`PhyStatusCtx`] (from [`phy_status_ctx`]).
//! The per-path statistics upstream also folds into `dm_info` are a
//! separate step ([`update_dm`]), so the parser itself stays a pure
//! function of its inputs (and fuzzable).

use crate::dev::{Bw, Dev, RF_PATH_A, RF_PATH_B};
use crate::error::{Error, Result};
use crate::hci::Hci;
use crate::phy::rate::{DESC_RATE11M, DESC_RATEMCS0};
use crate::phy::rssi::rf_power_2_rssi;
use crate::phy::{ch_5g_band, is_ch_2g};
use crate::rtw8822c::DmState;
use crate::rx::RxPktStat;

/// Bytes page 0 reads (up to `GAIN_B` in dword 4).
pub const PAGE0_LEN: usize = 20;
/// Bytes page 1 reads (up to `RXSNR_B` in dword 6).
pub const PAGE1_LEN: usize = 26;

const MIN_RX_POWER: i8 = -120;

/// What the parser needs from the device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhyStatusCtx {
    /// `dm_info->cck_gi_l_bnd` / `cck_gi_u_bnd`, read in `phy_set_param`.
    pub cck_gi_l_bnd: u8,
    pub cck_gi_u_bnd: u8,
    /// `hal->current_band_width`, for page 1 frames with sub-channel 0.
    pub current_bw: Bw,
}

/// The context for `dev`'s current state.
pub fn phy_status_ctx<H: Hci>(dev: &Dev<H>) -> PhyStatusCtx {
    PhyStatusCtx {
        cck_gi_l_bnd: dev.dm.cck_gi_l_bnd,
        cck_gi_u_bnd: dev.dm.cck_gi_u_bnd,
        current_bw: dev.hal.current_band_width,
    }
}

/// Upstream `rtw_set_rx_freq_band`: the band (0 = 2.4 GHz, 1 = 5 GHz, as
/// `NL80211_BAND_*`) and centre frequency of `channel`; unknown channels
/// leave both alone.
pub fn set_rx_freq_band(pkt: &mut RxPktStat, channel: u8) {
    if is_ch_2g(channel) {
        pkt.band = 0;
        pkt.freq = if channel == 14 {
            2484
        } else {
            2407 + 5 * u16::from(channel)
        };
    } else if ch_5g_band(channel).is_some() {
        pkt.band = 1;
        pkt.freq = 5000 + 5 * u16::from(channel);
    }
}

/// Upstream `query_phy_status`: fill `pkt` from the PHY status in `ps`.
/// Errors (and leaves `pkt` alone) on a short buffer or an unknown page,
/// where upstream only warns.
pub fn query_phy_status(ctx: &PhyStatusCtx, ps: &[u8], pkt: &mut RxPktStat) -> Result<()> {
    let page = ps.first().ok_or(Error::Invalid("empty phy status"))? & 0xf;
    match page {
        0 if ps.len() >= PAGE0_LEN => page0(ctx, ps, pkt),
        1 if ps.len() >= PAGE1_LEN => page1(ctx, ps, pkt),
        0 | 1 => return Err(Error::Invalid("short phy status")),
        _ => return Err(Error::Invalid("phy status page")),
    }
    Ok(())
}

/// Upstream `query_phy_status_page0` (CCK): the reported power corrected by
/// how far the AGC gain index sat outside the calibrated CCK window.
fn page0(ctx: &PhyStatusCtx, ps: &[u8], pkt: &mut RxPktStat) {
    let mut rx_power = [ps[1] as i8, ps[16] as i8];
    let gains = [ps[2] & 0x3f, ps[19] & 0x3f];
    let (l_bnd, u_bnd) = (ctx.cck_gi_l_bnd, ctx.cck_gi_u_bnd);
    for (p, &g) in rx_power.iter_mut().zip(gains.iter()) {
        // s8 arithmetic with truncation, as upstream's `+=`/`-=` on s8.
        if g < l_bnd {
            *p = p.wrapping_add((l_bnd.wrapping_sub(g) << 1) as i8);
        } else if g > u_bnd {
            *p = p.wrapping_sub((g.wrapping_sub(u_bnd) << 1) as i8);
        }
        *p = p.wrapping_sub(110);
    }
    let channel = ps[6];
    if channel != 0 {
        set_rx_freq_band(pkt, channel);
    } else {
        pkt.channel_invalid = true;
    }
    pkt.rx_power = rx_power;
    pkt.rssi = rf_power_2_rssi(&pkt.rx_power, 1);
    pkt.bw = Bw::Mhz20 as u8;
    pkt.signal_power = i32::from(rx_power[RF_PATH_A].max(MIN_RX_POWER));
}

/// Upstream `query_phy_status_page1` (OFDM/HT/VHT).
fn page1(ctx: &PhyStatusCtx, ps: &[u8], pkt: &mut RxPktStat) {
    let legacy = pkt.rate > DESC_RATE11M && pkt.rate < DESC_RATEMCS0;
    let rxsc = if legacy { ps[5] & 0xf } else { ps[5] >> 4 };
    let bw = match rxsc {
        0 => ctx.current_bw,
        1..=8 => Bw::Mhz20,
        9..=12 => Bw::Mhz40,
        _ => Bw::Mhz80,
    };
    set_rx_freq_band(pkt, ps[6]);
    pkt.rx_power = [ps[1].wrapping_sub(110) as i8, ps[2].wrapping_sub(110) as i8];
    pkt.rssi = rf_power_2_rssi(&pkt.rx_power, 2);
    pkt.bw = bw as u8;
    pkt.signal_power = i32::from(
        pkt.rx_power[RF_PATH_A]
            .max(pkt.rx_power[RF_PATH_B])
            .max(MIN_RX_POWER),
    );
    pkt.rx_evm = [ps[16], ps[17]];
    pkt.rx_snr = [ps[24] as i8, ps[25] as i8];
    pkt.cfo_tail = [ps[20] as i8, ps[21] as i8];
}

/// The `dm_info` side of upstream's page parsers: per-path RSSI, and for
/// page 1 frames SNR, CFO and EVM in dB, plus the last RX rate. (Path
/// diversity sums and CFO tracking by BSSID are dynamic mechanisms this
/// driver does not run yet.)
pub fn update_dm(dm: &mut DmState, rf_path_num: u8, pkt: &RxPktStat, page1: bool) {
    let paths = usize::from(rf_path_num).min(pkt.rx_power.len());
    // Upstream declares `evm_dbm` outside its loop: a path with a
    // non-negative EVM reports the previous path's value.
    let mut evm_dbm = 0u8;
    for path in 0..paths {
        dm.rssi[path] = rf_power_2_rssi(&pkt.rx_power[path..], 1);
        if !page1 {
            continue;
        }
        dm.rx_snr[path] = pkt.rx_snr[path] >> 1;
        dm.cfo_tail[path] = (i16::from(pkt.cfo_tail[path]) * 5) >> 1;
        let evm = pkt.rx_evm[path] as i8;
        if evm < 0 {
            evm_dbm = if evm == i8::MIN {
                0
            } else {
                (evm as u8).wrapping_neg() >> 1
            };
        }
        dm.rx_evm_dbm[path] = evm_dbm;
    }
    if page1 {
        dm.curr_rx_rate = pkt.rate;
    }
}
