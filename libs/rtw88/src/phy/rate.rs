//! Hardware rate codes and rate sections (upstream `enum rtw_rate_index`
//! / `DESC_RATE*` in `main.h`, `rtw_rate_section`/`rtw_rate_size` in
//! `phy.c`).

pub const DESC_RATE1M: u8 = 0x00;
pub const DESC_RATE2M: u8 = 0x01;
pub const DESC_RATE5_5M: u8 = 0x02;
pub const DESC_RATE11M: u8 = 0x03;
pub const DESC_RATE6M: u8 = 0x04;
pub const DESC_RATE9M: u8 = 0x05;
pub const DESC_RATE12M: u8 = 0x06;
pub const DESC_RATE18M: u8 = 0x07;
pub const DESC_RATE24M: u8 = 0x08;
pub const DESC_RATE36M: u8 = 0x09;
pub const DESC_RATE48M: u8 = 0x0a;
pub const DESC_RATE54M: u8 = 0x0b;
pub const DESC_RATEMCS0: u8 = 0x0c;
pub const DESC_RATEMCS1: u8 = 0x0d;
pub const DESC_RATEMCS7: u8 = 0x13;
pub const DESC_RATEMCS8: u8 = 0x14;
pub const DESC_RATEMCS9: u8 = 0x15;
pub const DESC_RATEMCS15: u8 = 0x1b;
pub const DESC_RATEMCS16: u8 = 0x1c;
pub const DESC_RATEMCS23: u8 = 0x23;
pub const DESC_RATEMCS24: u8 = 0x24;
pub const DESC_RATEMCS31: u8 = 0x2b;
pub const DESC_RATEVHT1SS_MCS0: u8 = 0x2c;
pub const DESC_RATEVHT1SS_MCS1: u8 = 0x2d;
pub const DESC_RATEVHT1SS_MCS9: u8 = 0x35;
pub const DESC_RATEVHT2SS_MCS0: u8 = 0x36;
pub const DESC_RATEVHT2SS_MCS1: u8 = 0x37;
pub const DESC_RATEVHT2SS_MCS9: u8 = 0x3f;
pub const DESC_RATEVHT3SS_MCS0: u8 = 0x40;
pub const DESC_RATEVHT3SS_MCS9: u8 = 0x49;
pub const DESC_RATEVHT4SS_MCS0: u8 = 0x4a;
pub const DESC_RATEVHT4SS_MCS9: u8 = 0x53;
/// Upstream `DESC_RATE_MAX`: the number of rate codes.
pub const DESC_RATE_MAX: usize = 0x54;

/// Rate sections (upstream `enum rtw_rate_section`).
pub const RS_CCK: usize = 0;
pub const RS_OFDM: usize = 1;
pub const RS_HT_1S: usize = 2;
pub const RS_HT_2S: usize = 3;
pub const RS_VHT_1S: usize = 4;
pub const RS_VHT_2S: usize = 5;
/// Upstream `__RTW_RATE_SECTION_2SS_MAX`: the last section a 2T2R chip uses.
pub const RS_2SS_MAX: usize = RS_VHT_2S;
pub const RS_HT_3S: usize = 6;
pub const RS_HT_4S: usize = 7;
pub const RS_VHT_3S: usize = 8;
pub const RS_VHT_4S: usize = 9;
pub const RATE_SECTION_NUM: usize = 10;

const fn span<const N: usize>(first: u8) -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = first + i as u8;
        i += 1;
    }
    out
}

const CCK: [u8; 4] = span(DESC_RATE1M);
const OFDM: [u8; 8] = span(DESC_RATE6M);
const HT_1S: [u8; 8] = span(DESC_RATEMCS0);
const HT_2S: [u8; 8] = span(DESC_RATEMCS8);
const VHT_1S: [u8; 10] = span(DESC_RATEVHT1SS_MCS0);
const VHT_2S: [u8; 10] = span(DESC_RATEVHT2SS_MCS0);
const HT_3S: [u8; 8] = span(DESC_RATEMCS16);
const HT_4S: [u8; 8] = span(DESC_RATEMCS24);
const VHT_3S: [u8; 10] = span(DESC_RATEVHT3SS_MCS0);
const VHT_4S: [u8; 10] = span(DESC_RATEVHT4SS_MCS0);

/// Upstream `rtw_rate_section[]` (with `rtw_rate_size[]` as each slice's
/// length), in section order.
pub const RATE_SECTION: [&[u8]; RATE_SECTION_NUM] =
    [&CCK, &OFDM, &HT_1S, &HT_2S, &VHT_1S, &VHT_2S, &HT_3S, &HT_4S, &VHT_3S, &VHT_4S];

/// Upstream `rtw_phy_rate_to_rate_section`; `None` for a code past the table
/// (upstream returns `RTW_RATE_SECTION_NUM`).
pub fn rate_section(rate: u8) -> Option<usize> {
    Some(match rate {
        DESC_RATE1M..=DESC_RATE11M => RS_CCK,
        DESC_RATE6M..=DESC_RATE54M => RS_OFDM,
        DESC_RATEMCS0..=DESC_RATEMCS7 => RS_HT_1S,
        DESC_RATEMCS8..=DESC_RATEMCS15 => RS_HT_2S,
        DESC_RATEMCS16..=DESC_RATEMCS23 => RS_HT_3S,
        DESC_RATEMCS24..=DESC_RATEMCS31 => RS_HT_4S,
        DESC_RATEVHT1SS_MCS0..=DESC_RATEVHT1SS_MCS9 => RS_VHT_1S,
        DESC_RATEVHT2SS_MCS0..=DESC_RATEVHT2SS_MCS9 => RS_VHT_2S,
        DESC_RATEVHT3SS_MCS0..=DESC_RATEVHT3SS_MCS9 => RS_VHT_3S,
        DESC_RATEVHT4SS_MCS0..=DESC_RATEVHT4SS_MCS9 => RS_VHT_4S,
        _ => return None,
    })
}
