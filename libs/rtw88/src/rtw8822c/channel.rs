//! Channel switching (upstream `rtw8822c_set_channel` and its
//! `_bb`/`_rf` halves, `rtw8822c_toggle_igi`, `rtw8822c_rstb_3wire`).
//!
//! Order, as upstream: the BB (band filters, CCK on/off, AGC tables, the
//! width), the MAC (`rtw_set_channel_mac`, owned by the MAC package), the
//! RF synthesiser (register 0x18 on both paths, with the 3-wire interface
//! held in reset), then an IGI toggle so the AGC re-converges.

use crate::dev::{Bw, Dev, RF_PATH_A, RF_PATH_B};
use crate::hci::{Hci, RegExt};
use crate::phy::rf::{read_rf, write_rf, RFREG_MASK};
use crate::phy::{ch_5g_band, is_ch_2g};
use crate::rtw8822c::regs::*;
use crate::rtw8822c::standin;

/// Upstream `rtw8822c_set_channel`. `primary_chan_idx` is the 20 MHz
/// sub-channel (`RTW_SC_*`, 0 at 20 MHz).
pub fn set_channel<H: Hci>(dev: &mut Dev<H>, channel: u8, bw: Bw, primary_chan_idx: u8) {
    set_channel_bb(dev, channel, bw, primary_chan_idx);
    standin::set_channel_mac(dev, channel, bw, primary_chan_idx);
    set_channel_rf(dev, channel, bw);
    toggle_igi(dev);
}

/// Upstream `rtw8822c_toggle_igi`: step the initial gain down by 2 and
/// back on both paths.
pub fn toggle_igi<H: Hci>(dev: &mut Dev<H>) {
    let igi = dev.hci.read32_mask(REG_RXIGI, 0x7f);
    let h = &mut dev.hci;
    h.write32_mask(REG_RXIGI, 0x7f, igi.wrapping_sub(2));
    h.write32_mask(REG_RXIGI, 0x7f00, igi.wrapping_sub(2));
    h.write32_mask(REG_RXIGI, 0x7f, igi);
    h.write32_mask(REG_RXIGI, 0x7f00, igi);
}

fn set_channel_bb_2g<H: Hci>(h: &mut H, channel: u8, bw: Bw) {
    h.clr32(REG_BGCTRL, BITS_RX_IQ_WEIGHT);
    h.set32(REG_TXF4, bit(20));
    h.clr32(REG_CCK_CHECK, BIT_CHECK_CCK_EN);
    h.clr32(REG_CCKTXONLY, BIT_BB_CCK_CHECK_EN);
    h.write32_mask(REG_CCAMSK, 0x3f00_0000, 0xf);
    let agc = match bw {
        Bw::Mhz20 => Some((0x5, 0x6)),
        Bw::Mhz40 => Some((0x4, 0x0)),
        Bw::Mhz80 => None,
    };
    if let Some((cck, ofdm)) = agc {
        h.write32_mask(REG_RXAGCCTL0, BITS_RXAGC_CCK, cck);
        h.write32_mask(REG_RXAGCCTL, BITS_RXAGC_CCK, cck);
        h.write32_mask(REG_RXAGCCTL0, BITS_RXAGC_OFDM, ofdm);
        h.write32_mask(REG_RXAGCCTL, BITS_RXAGC_OFDM, ofdm);
    }
    let scotrk = match channel {
        13 | 14 => 0x969,
        11 | 12 => 0x96a,
        _ => 0x9aa,
    };
    h.write32_mask(REG_SCOTRK, 0xfff, scotrk);
    // The CCK TX filter: channel 14 (Japan, CCK only) has its own shape.
    let txf: [u32; 8] = if channel == 14 {
        [0x3da0, 0x4962c931, 0x6aa3, 0xaa7b, 0xf3d7, 0x0, 0xff012455, 0xffff]
    } else {
        [0x5284, 0x3e18fec8, 0x0a88, 0xacc4, 0xc8b2, 0x00faf0de, 0x00122344, 0x0fffffff]
    };
    h.write32_mask(REG_TXF0, MASKHWORD, txf[0]);
    h.write32_mask(REG_TXF1, MASKDWORD, txf[1]);
    h.write32_mask(REG_TXF2, MASKLWORD, txf[2]);
    h.write32_mask(REG_TXF3, MASKHWORD, txf[3]);
    h.write32_mask(REG_TXF4, MASKLWORD, txf[4]);
    h.write32_mask(REG_TXF5, MASKDWORD, txf[5]);
    h.write32_mask(REG_TXF6, MASKDWORD, txf[6]);
    h.write32_mask(REG_TXF7, MASKDWORD, txf[7]);
    h.write32_mask(REG_TXDFIR0, 0x70, if channel == 13 { 0x3 } else { 0x1 });
}

fn set_channel_bb_5g<H: Hci>(h: &mut H, channel: u8, band: u8) {
    h.set32(REG_CCKTXONLY, BIT_BB_CCK_CHECK_EN);
    h.set32(REG_CCK_CHECK, BIT_CHECK_CCK_EN);
    h.set32(REG_BGCTRL, BITS_RX_IQ_WEIGHT);
    h.clr32(REG_TXF4, bit(20));
    h.write32_mask(REG_CCAMSK, 0x3f00_0000, 0x22);
    h.write32_mask(REG_TXDFIR0, 0x70, 0x3);
    let ofdm = match band {
        1 | 2 => 0x1,
        3 => 0x2,
        _ => 0x3,
    };
    h.write32_mask(REG_RXAGCCTL0, BITS_RXAGC_OFDM, ofdm);
    h.write32_mask(REG_RXAGCCTL, BITS_RXAGC_OFDM, ofdm);
    let scotrk = match channel {
        36..=51 => Some(0x494),
        52..=55 => Some(0x493),
        56..=111 => Some(0x453),
        112..=119 => Some(0x452),
        120..=172 => Some(0x412),
        173..=177 => Some(0x411),
        _ => None,
    };
    if let Some(v) = scotrk {
        h.write32_mask(REG_SCOTRK, 0xfff, v);
    }
}

/// Upstream `rtw8822c_set_channel_bb` (the 5/10 MHz widths are not
/// offered).
pub fn set_channel_bb<H: Hci>(dev: &mut Dev<H>, channel: u8, bw: Bw, primary_ch_idx: u8) {
    let h = &mut dev.hci;
    if is_ch_2g(channel) {
        set_channel_bb_2g(h, channel, bw);
    } else if let Some(band) = ch_5g_band(channel) {
        set_channel_bb_5g(h, channel, band);
    }
    match bw {
        Bw::Mhz20 => {
            h.write32_mask(REG_DFIRBW, 0x3ff0, 0x19b);
            h.write32_mask(REG_TXBWCTL, 0xf, 0x0);
            h.write32_mask(REG_TXBWCTL, 0xffc0, 0x0);
            h.write32_mask(REG_TXCLK, 0x700, 0x7);
            h.write32_mask(REG_TXCLK, 0x70_0000, 0x6);
            h.write32_mask(REG_CCK_SOURCE, BIT_NBI_EN, 0x0);
            h.write32_mask(REG_SBD, BITS_SUBTUNE, 0x1);
            h.write32_mask(REG_PT_CHSMO, BIT_PT_OPT, 0x0);
        }
        Bw::Mhz40 => {
            h.write32_mask(REG_CCKSB, bit(4), u32::from(primary_ch_idx == RTW_SC_20_UPPER));
            h.write32_mask(REG_TXBWCTL, 0xf, 0x5);
            h.write32_mask(REG_TXBWCTL, 0xc0, 0x0);
            h.write32_mask(REG_TXBWCTL, 0xff00, u32::from(primary_ch_idx | (primary_ch_idx << 4)));
            h.write32_mask(REG_CCK_SOURCE, BIT_NBI_EN, 0x1);
            h.write32_mask(REG_SBD, BITS_SUBTUNE, 0x1);
            h.write32_mask(REG_PT_CHSMO, BIT_PT_OPT, 0x1);
        }
        Bw::Mhz80 => {
            h.write32_mask(REG_TXBWCTL, 0xf, 0xa);
            h.write32_mask(REG_TXBWCTL, 0xc0, 0x0);
            h.write32_mask(REG_TXBWCTL, 0xff00, u32::from(primary_ch_idx | (primary_ch_idx << 4)));
            h.write32_mask(REG_SBD, BITS_SUBTUNE, 0x6);
            h.write32_mask(REG_PT_CHSMO, BIT_PT_OPT, 0x1);
        }
    }
}

/// Upstream `rtw8822c_rstb_3wire`: hold (`false`) or release the BB-RF
/// 3-wire interface; on release both paths re-latch their analog settings.
pub fn rstb_3wire<H: Hci>(h: &mut H, enable: bool) {
    if enable {
        h.write32_mask(REG_RSTB, BIT_RSTB_3WIRE, 0x1);
        h.write32_mask(REG_ANAPAR_A, BIT_ANAPAR_UPDATE, 0x1);
        h.write32_mask(REG_ANAPAR_B, BIT_ANAPAR_UPDATE, 0x1);
    } else {
        h.write32_mask(REG_RSTB, BIT_RSTB_3WIRE, 0x0);
    }
}

const RF18_BAND_MASK: u32 = bit(16) | bit(9) | bit(8);
const RF18_BAND_5G: u32 = bit(16) | bit(8);
const RF18_CHANNEL_MASK: u32 = MASKBYTE0;
const RF18_RFSI_MASK: u32 = bit(18) | bit(17);
const RF18_RFSI_GE_CH80: u32 = bit(17);
const RF18_RFSI_GT_CH140: u32 = bit(18);
const RF18_BW_MASK: u32 = bit(13) | bit(12);
const RF18_BW_20M: u32 = bit(13) | bit(12);
const RF18_BW_40M: u32 = bit(13);
const RF18_BW_80M: u32 = bit(12);

/// The RF register 0x18 value for `channel` and `bw`, from path A's
/// current one (upstream computes it inline).
pub fn rf_reg18(old: u32, channel: u8, bw: Bw) -> u32 {
    let mut v = old & !(RF18_BAND_MASK | RF18_CHANNEL_MASK | RF18_RFSI_MASK | RF18_BW_MASK);
    if !is_ch_2g(channel) {
        v |= RF18_BAND_5G;
    }
    v |= u32::from(channel) & RF18_CHANNEL_MASK;
    match ch_5g_band(channel) {
        Some(4) => v |= RF18_RFSI_GT_CH140,
        Some(3) => v |= RF18_RFSI_GE_CH80,
        _ => {}
    }
    v | match bw {
        Bw::Mhz20 => RF18_BW_20M,
        Bw::Mhz40 => RF18_BW_40M,
        Bw::Mhz80 => RF18_BW_80M,
    }
}

/// Upstream `rtw8822c_set_channel_rf`: the RX baseband filter LUT entry
/// 0x12 for the width, then register 0x18 (band, channel, RF section,
/// width) on both paths.
pub fn set_channel_rf<H: Hci>(dev: &mut Dev<H>, channel: u8, bw: Bw) {
    let old = read_rf(dev, RF_PATH_A, RF_CFGCH, RFREG_MASK);
    let reg18 = rf_reg18(old, channel, bw);
    let rxbb = match bw {
        Bw::Mhz20 => 0x18,
        Bw::Mhz40 => 0x10,
        Bw::Mhz80 => 0x8,
    };
    rstb_3wire(&mut dev.hci, false);
    for path in [RF_PATH_A, RF_PATH_B] {
        write_rf(dev, path, RF_LUTWE2, 0x04, 0x01);
        write_rf(dev, path, RF_LUTWA, 0x1f, 0x12);
        write_rf(dev, path, RF_LUTWD0, 0xfffff, rxbb);
        write_rf(dev, path, RF_LUTWE2, 0x04, 0x00);
    }
    write_rf(dev, RF_PATH_A, RF_CFGCH, RFREG_MASK, reg18);
    write_rf(dev, RF_PATH_B, RF_CFGCH, RFREG_MASK, reg18);
    rstb_3wire(&mut dev.hci, true);
}
