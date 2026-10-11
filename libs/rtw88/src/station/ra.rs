//! The AP's capabilities and what the firmware's rate adaptation is told
//! about them (upstream `main.c`: `rtw_update_sta_info`, `get_rate_id`,
//! `get_vht_ra_mask`, `rtw_rate_mask_rssi`, `rtw_rate_mask_recover`,
//! `rtw_update_channel`).
//!
//! Upstream reads mac80211's `ieee80211_sta`; here the soft-MAC hands a
//! [`PeerCaps`] decoded from the association response (IEEE 802.11-2020
//! 9.4.2.56 HT Capabilities, 9.4.2.157 VHT Capabilities).

use crate::dev::{Band, Bw, Dev, Hal};
use crate::error::{Error, Result};
use crate::fw::RaInfo;
use crate::hci::Hci;

/// HT Capabilities of the AP (the parts rate adaptation reads).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HtCaps {
    /// HT Capability Information field.
    pub cap: u16,
    /// Rx MCS bitmask, first four bytes (MCS 0-31).
    pub rx_mask: [u8; 4],
    /// A-MPDU parameters: maximum length exponent and minimum spacing.
    pub ampdu_factor: u8,
    pub ampdu_density: u8,
}

/// VHT Capabilities of the AP (the parts rate adaptation reads).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VhtCaps {
    /// VHT Capabilities Information field.
    pub cap: u32,
    pub rx_mcs_map: u16,
    pub tx_mcs_map: u16,
}

/// What the soft-MAC knows of the AP after association.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerCaps {
    /// Supported legacy rates as a bitmap in the band's rate order:
    /// 2.4 GHz bit 0-11 = 1, 2, 5.5, 11, 6, 9, 12, 18, 24, 36, 48, 54 Mb/s;
    /// 5 GHz bit 0-7 = 6, 9, 12, 18, 24, 36, 48, 54 Mb/s.
    pub supp_rates: u32,
    /// The basic rates, same order.
    pub basic_rates: u32,
    pub ht: Option<HtCaps>,
    pub vht: Option<VhtCaps>,
    /// The width the AP receives with.
    pub bw: Bw,
}

// Capability bits (IEEE 802.11-2020 Figures 9-421 and 9-610).
const HT_CAP_LDPC_CODING: u16 = 1 << 0;
const HT_CAP_SGI_20: u16 = 1 << 5;
const HT_CAP_SGI_40: u16 = 1 << 6;
const HT_CAP_RX_STBC: u16 = 0x0300;
const VHT_CAP_RXLDPC: u32 = 1 << 4;
const VHT_CAP_SHORT_GI_80: u32 = 1 << 5;
const VHT_CAP_RXSTBC_MASK: u32 = 0x0700;

const HT_STBC_EN: u8 = 1 << 0;
const VHT_STBC_EN: u8 = 1 << 1;
const HT_LDPC_EN: u8 = 1 << 0;
const VHT_LDPC_EN: u8 = 1 << 1;

/// `WIRELESS_*`.
const W_CCK: u8 = 1;
const W_OFDM: u8 = 2;
const W_HT: u8 = 4;
const W_VHT: u8 = 8;

const RA_MASK_CCK_RATES: u64 = 0x0000f;
const RA_MASK_OFDM_RATES: u64 = 0x00ff0;
const RA_MASK_HT_RATES_1SS: u64 = 0xff000;
const RA_MASK_HT_RATES_2SS: u64 = 0xff000 << 8;
const RA_MASK_HT_RATES_3SS: u64 = 0xff000 << 16;
const RA_MASK_HT_RATES: u64 = RA_MASK_HT_RATES_1SS | RA_MASK_HT_RATES_2SS | RA_MASK_HT_RATES_3SS;
const RA_MASK_VHT_RATES_1SS: u64 = 0x3ff000;
const RA_MASK_VHT_RATES_2SS: u64 = 0x3ff000 << 10;
const RA_MASK_VHT_RATES_3SS: u64 = 0x3ff000 << 20;
const RA_MASK_VHT_RATES: u64 = RA_MASK_VHT_RATES_1SS | RA_MASK_VHT_RATES_2SS | RA_MASK_VHT_RATES_3SS;
const RA_MASK_CCK_IN_BG: u64 = 0x00005;
const RA_MASK_CCK_IN_HT: u64 = 0x00005;
const RA_MASK_CCK_IN_VHT: u64 = 0x00005;
const RA_MASK_OFDM_IN_VHT: u64 = 0x00010;
const RA_MASK_OFDM_IN_HT_2G: u64 = 0x00010;
const RA_MASK_OFDM_IN_HT_5G: u64 = 0x00030;

/// Rate ids (upstream `enum rtw_rate_index`).
pub mod rate_id {
    pub const BGN_40M_2SS: u8 = 0;
    pub const BGN_40M_1SS: u8 = 1;
    pub const BGN_20M_2SS: u8 = 2;
    pub const BGN_20M_1SS: u8 = 3;
    pub const GN_N2SS: u8 = 4;
    pub const GN_N1SS: u8 = 5;
    pub const BG: u8 = 6;
    pub const G: u8 = 7;
    pub const B_20M: u8 = 8;
    pub const ARFR0_AC_2SS: u8 = 9;
    pub const ARFR1_AC_1SS: u8 = 10;
    pub const ARFR2_AC_2G_1SS: u8 = 11;
    pub const ARFR3_AC_2G_2SS: u8 = 12;
    pub const ARFR4_AC_3SS: u8 = 13;
    pub const ARFR5_N_3SS: u8 = 14;
    pub const ARFR7_N_4SS: u8 = 15;
    pub const ARFR6_AC_4SS: u8 = 16;
}

/// The AP as the driver keeps it (upstream `struct rtw_sta_info`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StaInfo {
    pub mac_id: u8,
    pub rate_id: u8,
    pub init_ra_lv: u8,
    pub rssi_level: u8,
    pub bw_mode: Bw,
    pub stbc_en: u8,
    pub ldpc_en: u8,
    pub sgi_enable: bool,
    pub vht_enable: bool,
    pub ra_mask: u64,
    pub peer: PeerCaps,
    /// The last rate the firmware reported (`C2H_RA_RPT`, a `DESC_RATE*`).
    pub tx_rate: u8,
    pub tx_sgi: bool,
    pub tx_bw: u8,
}

impl StaInfo {
    /// The rate-adaptation H2C fields.
    pub fn ra_info(&self) -> RaInfo {
        RaInfo {
            mac_id: self.mac_id,
            rate_id: self.rate_id,
            init_ra_lv: self.init_ra_lv,
            sgi_enable: self.sgi_enable,
            bw_mode: self.bw_mode as u8,
            ldpc_en: self.ldpc_en,
            vht_enable: self.vht_enable,
            ra_mask: self.ra_mask,
        }
    }
}

/// Upstream `get_vht_ra_mask`: two bits per stream give its top MCS.
fn get_vht_ra_mask(mut mcs_map: u16) -> u64 {
    let mut ra_mask = 0u64;
    let mut nss = 12;
    for _ in 0..4 {
        match mcs_map & 0x3 {
            2 => ra_mask |= 0x3ff << nss,
            1 => ra_mask |= 0x1ff << nss,
            0 => ra_mask |= 0x0ff << nss,
            _ => {}
        }
        mcs_map >>= 2;
        nss += 10;
    }
    ra_mask
}

/// Upstream `get_rate_id`.
pub fn get_rate_id(wireless_set: u8, bw: Bw, tx_num: u8) -> u8 {
    use rate_id::*;
    let by_nss = |ids: [u8; 4]| match tx_num {
        1..=4 => ids[usize::from(tx_num) - 1],
        _ => 0,
    };
    match wireless_set {
        W_CCK => B_20M,
        W_OFDM => G,
        x if x == W_CCK | W_OFDM => BG,
        x if x == W_OFDM | W_HT => by_nss([GN_N1SS, GN_N2SS, ARFR5_N_3SS, 0]),
        x if x == W_CCK | W_OFDM | W_HT => match bw {
            Bw::Mhz40 => by_nss([BGN_40M_1SS, BGN_40M_2SS, ARFR5_N_3SS, ARFR7_N_4SS]),
            _ => by_nss([BGN_20M_1SS, BGN_20M_2SS, ARFR5_N_3SS, ARFR7_N_4SS]),
        },
        x if x == W_OFDM | W_VHT => by_nss([ARFR1_AC_1SS, ARFR0_AC_2SS, ARFR4_AC_3SS, ARFR6_AC_4SS]),
        x if x == W_CCK | W_OFDM | W_VHT => match bw {
            Bw::Mhz80 => by_nss([ARFR1_AC_1SS, ARFR0_AC_2SS, ARFR4_AC_3SS, ARFR6_AC_4SS]),
            _ => by_nss([ARFR2_AC_2G_1SS, ARFR3_AC_2G_2SS, ARFR4_AC_3SS, ARFR6_AC_4SS]),
        },
        _ => 0,
    }
}

/// Upstream `rtw_rate_mask_rssi`: drop the slowest rates as the signal grows.
fn rate_mask_rssi(rssi_level: u8, wireless_set: u8) -> u64 {
    if wireless_set == W_CCK {
        return u64::MAX;
    }
    match rssi_level {
        0 => u64::MAX,
        1 => 0xffff_ffff_ffff_fff0,
        2 => 0xffff_ffff_ffff_efe0,
        3 => 0xffff_ffff_ffff_cfc0,
        4 => 0xffff_ffff_ffff_8f80,
        _ => 0xffff_ffff_ffff_0f00,
    }
}

/// Upstream `rtw_rate_mask_recover`.
fn rate_mask_recover(mut ra_mask: u64, bak: u64) -> u64 {
    let legacy = RA_MASK_CCK_RATES | RA_MASK_OFDM_RATES;
    if ra_mask & !legacy == 0 {
        ra_mask |= bak & !legacy;
    }
    if ra_mask == 0 {
        ra_mask |= bak & legacy;
    }
    ra_mask
}

/// Upstream `rtw_update_sta_info` (without the user rate mask, which this
/// driver does not have): the rate set, width, STBC/LDPC and short GI the
/// firmware may use towards the AP. `init_ra_lv` is 1, as `rtw_sta_add`
/// sets it for the first RA info.
pub fn update_sta_info<H: Hci>(dev: &Dev<H>, mac_id: u8, peer: &PeerCaps) -> StaInfo {
    let hal = &dev.hal;
    let nss_cap = match dev.efuse.hw_cap.nss {
        0 => hal.rf_path_num.max(1),
        n => n,
    };
    let (mut ra_mask, mut stbc_en, mut ldpc_en) = (0u64, 0u8, 0u8);
    if let Some(vht) = &peer.vht {
        ra_mask |= get_vht_ra_mask(vht.rx_mcs_map);
        stbc_en = if vht.cap & VHT_CAP_RXSTBC_MASK != 0 { VHT_STBC_EN } else { 0 };
        ldpc_en = if vht.cap & VHT_CAP_RXLDPC != 0 { VHT_LDPC_EN } else { 0 };
    } else if let Some(ht) = &peer.ht {
        let m = ht.rx_mask.map(u64::from);
        ra_mask |= (m[3] << 36) | (m[2] << 28) | (m[1] << 20) | (m[0] << 12);
        stbc_en = if ht.cap & HT_CAP_RX_STBC != 0 { HT_STBC_EN } else { 0 };
        ldpc_en = if ht.cap & HT_CAP_LDPC_CODING != 0 { HT_LDPC_EN } else { 0 };
    }

    if nss_cap == 1 || hal.txrx_1ss {
        ra_mask &= RA_MASK_VHT_RATES_1SS | RA_MASK_HT_RATES_1SS;
    } else if nss_cap == 2 {
        ra_mask &= RA_MASK_VHT_RATES_2SS | RA_MASK_HT_RATES_2SS | RA_MASK_VHT_RATES_1SS | RA_MASK_HT_RATES_1SS;
    }

    let supp = u64::from(peer.supp_rates);
    let (ra_mask_bak, wireless_set);
    match hal.current_band_type {
        Band::Ghz5 => {
            ra_mask |= supp << 4;
            ra_mask_bak = ra_mask;
            if peer.vht.is_some() {
                ra_mask &= RA_MASK_VHT_RATES | RA_MASK_OFDM_IN_VHT;
                wireless_set = W_OFDM | W_VHT;
            } else if peer.ht.is_some() {
                ra_mask &= RA_MASK_HT_RATES | RA_MASK_OFDM_IN_HT_5G;
                wireless_set = W_OFDM | W_HT;
            } else {
                wireless_set = W_OFDM;
            }
        }
        Band::Ghz2 => {
            ra_mask |= supp;
            ra_mask_bak = ra_mask;
            if peer.vht.is_some() {
                ra_mask &= RA_MASK_VHT_RATES | RA_MASK_CCK_IN_VHT | RA_MASK_OFDM_IN_VHT;
                wireless_set = W_CCK | W_OFDM | W_HT | W_VHT;
            } else if peer.ht.is_some() {
                ra_mask &= RA_MASK_HT_RATES | RA_MASK_CCK_IN_HT | RA_MASK_OFDM_IN_HT_2G;
                wireless_set = W_CCK | W_OFDM | W_HT;
            } else if supp <= 0xf {
                wireless_set = W_CCK;
            } else {
                ra_mask &= RA_MASK_OFDM_RATES | RA_MASK_CCK_IN_BG;
                wireless_set = W_CCK | W_OFDM;
            }
        }
    }

    let ht_cap = peer.ht.map_or(0, |h| h.cap);
    let (bw_mode, sgi) = match peer.bw {
        Bw::Mhz80 => (Bw::Mhz80, peer.vht.is_some_and(|v| v.cap & VHT_CAP_SHORT_GI_80 != 0)),
        Bw::Mhz40 => (Bw::Mhz40, peer.ht.is_some() && ht_cap & HT_CAP_SGI_40 != 0),
        Bw::Mhz20 => (Bw::Mhz20, peer.ht.is_some() && ht_cap & HT_CAP_SGI_20 != 0),
    };
    let tx_num = if peer.vht.is_some() || peer.ht.is_some() { nss_cap } else { 1 };
    let rate_id = get_rate_id(wireless_set, bw_mode, tx_num);

    let rssi_level = 0;
    ra_mask &= rate_mask_rssi(rssi_level, wireless_set);
    ra_mask = rate_mask_recover(ra_mask, ra_mask_bak);

    StaInfo {
        mac_id,
        rate_id,
        init_ra_lv: 1,
        rssi_level,
        bw_mode,
        stbc_en,
        ldpc_en,
        sgi_enable: sgi,
        vht_enable: peer.vht.is_some(),
        ra_mask,
        peer: *peer,
        tx_rate: 0,
        tx_sgi: false,
        tx_bw: bw_mode as u8,
    }
}

/// `ieee80211_channel_to_frequency` for the channels this chip tunes.
fn channel_to_freq(ch: u8, band: Band) -> Option<u32> {
    let ch = u32::from(ch);
    match band {
        Band::Ghz2 if ch == 14 => Some(2484),
        Band::Ghz2 if (1..14).contains(&ch) => Some(2407 + ch * 5),
        Band::Ghz5 if crate::mac::is_ch_5g_band(ch as u8) => Some(5000 + ch * 5),
        _ => None,
    }
}

/// Upstream `rtw_update_channel` (+ the band choice of `rtw_set_channel`):
/// record the channel in `hal` and return the primary channel's position
/// in the wide channel (`RTW_SC_*`).
pub fn update_channel(hal: &mut Hal, center: u8, primary: u8, bw: Bw) -> Result<u8> {
    const SC_DONT_CARE: u8 = 0;
    const SC_20_UPPER: u8 = 1;
    const SC_20_LOWER: u8 = 2;
    const SC_20_UPMOST: u8 = 3;
    const SC_20_LOWEST: u8 = 4;

    let band = if center > 14 { Band::Ghz5 } else { Band::Ghz2 };
    let (Some(center_freq), Some(primary_freq)) = (channel_to_freq(center, band), channel_to_freq(primary, band)) else {
        return Err(Error::Invalid("channel"));
    };
    hal.cch_by_bw[Bw::Mhz20 as usize] = primary;
    hal.cch_by_bw[bw as usize] = center;
    let idx = match bw {
        Bw::Mhz20 => SC_DONT_CARE,
        Bw::Mhz40 if primary_freq > center_freq => SC_20_UPPER,
        Bw::Mhz40 => SC_20_LOWER,
        Bw::Mhz80 if primary_freq > center_freq => {
            hal.cch_by_bw[Bw::Mhz40 as usize] = center.wrapping_add(4);
            if primary_freq - center_freq == 10 {
                SC_20_UPPER
            } else {
                SC_20_UPMOST
            }
        }
        Bw::Mhz80 => {
            hal.cch_by_bw[Bw::Mhz40 as usize] = center.wrapping_sub(4);
            if center_freq - primary_freq == 10 {
                SC_20_LOWER
            } else {
                SC_20_LOWEST
            }
        }
    };
    hal.current_primary_channel_index = idx;
    hal.current_band_width = bw;
    hal.primary_channel = primary;
    hal.current_channel = center;
    hal.current_band_type = band;
    Ok(idx)
}
