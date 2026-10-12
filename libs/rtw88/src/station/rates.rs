//! A BSS's legacy rates as the bitmaps [`super::PeerCaps`] and
//! [`super::Vif::basic_rates`] use: what mac80211 hands upstream as
//! `sta->supp_rates[band]` and `bss_conf.basic_rates`, read here from the
//! Supported Rates and Extended Supported Rates elements.

use crate::dev::Band;

/// The band's legacy rates in bitmap order, in 500 kb/s units: 2.4 GHz is
/// CCK 1, 2, 5.5, 11 then OFDM 6 to 54; 5 GHz is OFDM only.
const RATES_2G: [u8; 12] = [2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108];
const RATES_5G: [u8; 8] = [12, 18, 24, 36, 48, 72, 96, 108];

/// `(supported, basic)` bitmaps of the rate octets `octets` (bit 7 marks a
/// basic rate, IEEE 802.11-2020 9.4.2.3) in `band`. Octets that are not a
/// legacy rate of the band (BSS membership selectors, CCK on 5 GHz) are
/// ignored.
pub fn legacy_rates(band: Band, octets: impl IntoIterator<Item = u8>) -> (u32, u32) {
    let table: &[u8] = match band {
        Band::Ghz2 => &RATES_2G,
        Band::Ghz5 => &RATES_5G,
    };
    octets.into_iter().fold((0, 0), |(supp, basic), octet| {
        let Some(bit) = table.iter().position(|&r| r == octet & 0x7f) else {
            return (supp, basic);
        };
        let basic_bit = if octet & 0x80 != 0 { 1 << bit } else { 0 };
        (supp | 1 << bit, basic | basic_bit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_11b_compatible_bss() {
        // 1(B) 2(B) 5.5(B) 11(B) 6 9 12 18 | 24 36 48 54
        let rates = [0x82, 0x84, 0x8b, 0x96, 12, 18, 24, 36, 48, 72, 96, 108];
        assert_eq!(legacy_rates(Band::Ghz2, rates), (0xfff, 0x00f));
    }

    #[test]
    fn a_bss_without_cck_basic_rates() {
        // 6(B) 9 12(B) 18 24(B) 36 48 54, the HT selector (0xFF).
        let rates = [0x8c, 18, 0x98, 36, 0xb0, 72, 96, 108, 0xff];
        let (supp, basic) = legacy_rates(Band::Ghz2, rates);
        assert_eq!(supp, 0xff0);
        assert_eq!(basic, 0x150);
        assert_eq!(basic.trailing_zeros(), 4, "lowest basic is OFDM 6 Mb/s");
    }

    #[test]
    fn five_ghz_ignores_cck_and_selectors() {
        let rates = [0x82, 0x8c, 18, 0x98, 36, 0xb0, 72, 96, 108, 0xfe];
        assert_eq!(legacy_rates(Band::Ghz5, rates), (0xff, 0x15));
        assert_eq!(legacy_rates(Band::Ghz5, []), (0, 0));
    }
}
