//! Host tests: every element and frame type (valid, truncated, oversized,
//! duplicate, hidden SSID), RSN round trips, and the scan table.

use std::vec::Vec;

mod elements;
mod frames;
mod rsn;
mod table;

pub(crate) const AP: [u8; 6] = [0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
pub(crate) const STA: [u8; 6] = [0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE];

/// A realistic beacon element list: SSID, ten rates, DS channel, country,
/// HT capabilities, the WPA2-PSK RSN element, and a vendor element.
pub(crate) fn beacon_ies(ssid: &[u8], channel: u8) -> Vec<u8> {
    use crate::ie::IeBuilder;
    IeBuilder::new()
        .ssid(ssid)
        .unwrap()
        .rates(&[0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24, 0x30, 0x48])
        .unwrap()
        .ds_channel(channel)
        .unwrap()
        .raw(7, b"US \x01\x0b\x1e")
        .unwrap()
        .raw(45, &[0u8; 26])
        .unwrap()
        .rsn(&crate::Rsn::wpa2_psk())
        .unwrap()
        .raw(221, &[0x00, 0x50, 0xF2, 0x02, 0x01, 0x01])
        .unwrap()
        .finish()
}
