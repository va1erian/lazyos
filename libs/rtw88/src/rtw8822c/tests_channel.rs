//! `set_channel` at 20 MHz against register values worked out by hand
//! from upstream `rtw8822c_set_channel_bb/_rf`, `rtw_set_channel_mac` and
//! `rtw8822c_toggle_igi`, starting from a zeroed register file with only
//! the IGI (0x1d70 = 0x2020) and path A's RF 0x18 (0x93124: bit 19 kept,
//! channel 36 at 20 MHz in 5 GHz) preset.

use std::vec::Vec;

use super::set_channel;
use crate::dev::{Bw, Dev};
use crate::fake::FakeHci;
use crate::hci::Hci;

fn run(channel: u8) -> Dev<FakeHci> {
    let mut d = Dev::new(FakeHci::default());
    d.hal.rf_path_num = 2;
    d.hal.rf_phy_num = 2;
    d.hci.write32(0x1d70, 0x2020);
    d.hci.write32(0x3c60, 0x93124);
    d.hci.log.clear();
    set_channel(&mut d, channel, Bw::Mhz20, 0);
    d
}

/// Registers every 20 MHz switch ends with.
const BW20: [(u32, u32); 6] = [
    (0x810, 0x19b0),      // DFIRBW[13:4] = 0x19b
    (0x9b0, 0x0),         // TXBWCTL
    (0x9b4, 0x0060_0700), // TXCLK[10:8] = 7, [22:20] = 6
    (0x1abc, 0x0),        // CCK_SOURCE: NBI off
    (0x88c, 0x1000),      // SBD subtune 1
    (0xcbc, 0x0),         // PT_CHSMO: PT_OPT off
];

/// The RF side: LUT entry 0x12 = 0x18 (20 MHz RX filter), the write
/// enable back at 0, 3-wire released, both paths' ANAPAR re-latched, the
/// IGI back where it was.
const RF_COMMON: [(u32, u32); 9] = [
    (0x3c00 + 0xee * 4, 0x0),
    (0x3c00 + 0x33 * 4, 0x12),
    (0x3c00 + 0x3f * 4, 0x18),
    (0x4c00 + 0xee * 4, 0x0),
    (0x4c00 + 0x33 * 4, 0x12),
    (0x4c00 + 0x3f * 4, 0x18),
    (0x1c90, 0x100),
    (0x1830, 0x2000_0000),
    (0x1d70, 0x2020),
];

fn check(d: &Dev<FakeHci>, regs: &[(u32, u32)], what: &str) {
    for &(addr, want) in regs {
        assert_eq!(d.hci.peek32(addr), want, "{what}: reg {addr:#x}");
    }
}

fn channel_2g(ch: u8, scotrk: u32, rf18: u32) -> Vec<(u32, u32)> {
    std::vec![
        (0x1a14, 0),                 // RX IQ weight off
        (0x454, 0),                  // CCK check off (BB and MAC agree)
        (0x1a80, 0),                 // BB CCK check off
        (0x1c80, 0x0f00_0000),       // CCAMSK
        (0x18ac, 0x5060),            // RXAGCCTL0: CCK 5, OFDM 6
        (0x41ac, 0x5060),            // RXAGCCTL (path B)
        (0xc30, scotrk),             // SCOTRK
        (0x1a20, 0x5284_0000),       // CCK TX filter, channels 1-13
        (0x1a24, 0x3e18_fec8),
        (0x1a28, 0x0000_0a88),
        (0x1a98, 0xacc4_0000),
        (0x1a9c, 0x0010_c8b2),       // TXF4 with bit 20 set
        (0x1aa0, 0x00fa_f0de),
        (0x1aac, 0x0012_2344),
        (0x1ab0, 0x0fff_ffff),
        (0x808, if ch == 13 { 0x30 } else { 0x10 }),
        (0x3c60, rf18),
        (0x4c60, rf18),
    ]
}

fn channel_5g(ofdm_agc: u32, scotrk: u32, rf18: u32) -> Vec<(u32, u32)> {
    std::vec![
        (0x1a14, 0x300),         // RX IQ weight on
        (0x454, 0x80),           // CCK check on
        (0x1a80, 0x0004_0000),   // BB CCK check on
        (0x1c80, 0x2200_0000),   // CCAMSK
        (0x808, 0x30),           // TXDFIR0
        (0x18ac, ofdm_agc << 4), // OFDM AGC table, CCK untouched
        (0x41ac, ofdm_agc << 4),
        (0xc30, scotrk),
        (0x1a20, 0),             // the CCK filter is left alone
        (0x1a9c, 0),             // TXF4 bit 20 cleared
        (0x3c60, rf18),
        (0x4c60, rf18),
    ]
}

/// The MAC half (`rtw_set_channel_mac`): sub-channel 0, no RF mode bits,
/// MAC clock select 80 MHz (0), 80 in both microsecond counters.
const MAC: [(u32, u32); 3] = [(0x480, 0), (0x668, 0), (0x24, 0)];

#[test]
fn channels_1_6_11_at_20mhz() {
    // RF 0x18: bit 19 kept, band/RF-section/channel/width cleared, then
    // 2.4 GHz (0), the channel, 20 MHz (0x3000).
    for (ch, scotrk, rf18) in [(1u8, 0x9aa, 0x83001), (6, 0x9aa, 0x83006), (11, 0x96a, 0x8300b)] {
        let d = run(ch);
        check(&d, &channel_2g(ch, scotrk, rf18), &std::format!("ch {ch}"));
        check(&d, &BW20, "bw20");
        check(&d, &RF_COMMON, "rf");
        check(&d, &MAC, "mac");
        assert_eq!(d.hci.peek32(0x55c) & 0xff, 80);
        assert_eq!(d.hci.peek32(0x638) & 0xff, 80);
    }
}

#[test]
fn channels_36_and_149_at_20mhz() {
    // 36: band 1, OFDM AGC 1, SCOTRK 0x494; RF 0x18 = bit 19 | 5 GHz
    // (0x10100) | 20 MHz | 0x24.
    let d = run(36);
    check(&d, &channel_5g(1, 0x494, 0x93124), "ch 36");
    check(&d, &BW20, "bw20");
    check(&d, &RF_COMMON, "rf");
    // 149: band 4, OFDM AGC 3, SCOTRK 0x412; RF section "above 140" (bit 18).
    let d = run(149);
    check(&d, &channel_5g(3, 0x412, 0xd3195), "ch 149");
    check(&d, &BW20, "bw20");
    check(&d, &RF_COMMON, "rf");
}

#[test]
fn order_is_bb_mac_rf_with_3wire_held_and_igi_toggled() {
    let d = run(6);
    let w = d.hci.writes();
    let pos = |pred: &dyn Fn(u32, u32) -> bool| w.iter().position(|&(_, a, v)| pred(a, v)).unwrap();
    let bb = pos(&|a, _| a == 0x810);
    let mac = pos(&|a, _| a == 0x483);
    let hold = pos(&|a, v| a == 0x1c90 && v & 0x100 == 0);
    let rf = pos(&|a, _| a == 0x3c60);
    let release = w.iter().rposition(|&(_, a, v)| a == 0x1c90 && v & 0x100 != 0).unwrap();
    let igi_dip = pos(&|a, v| a == 0x1d70 && v & 0x7f == 0x1e);
    assert!(bb < mac && mac < hold && hold < rf && rf < release && release < igi_dip);
    // The IGI goes to 0x1e on both paths, then back to 0x20.
    let igi: Vec<u32> = w.iter().filter(|&&(_, a, _)| a == 0x1d70).map(|&(_, _, v)| v).collect();
    assert_eq!(igi, [0x201e, 0x1e1e, 0x1e20, 0x2020]);
}

#[test]
fn wider_channels_set_the_rf_width_and_sub_channel() {
    let mut d = Dev::new(FakeHci::default());
    d.hal.rf_path_num = 2;
    d.hal.rf_phy_num = 2;
    set_channel(&mut d, 38, Bw::Mhz40, super::regs::RTW_SC_20_LOWER);
    // RF: 5 GHz, channel 38, 40 MHz (bit 13); RX filter 0x10.
    assert_eq!(d.hci.peek32(0x3c60), 0x10100 | 0x2000 | 38);
    assert_eq!(d.hci.peek32(0x3c00 + 0x3f * 4), 0x10);
    // BB: TXBWCTL = 5 | sub-channel 2 in both nibbles of [15:8].
    assert_eq!(d.hci.peek32(0x9b0), 0x2205);
    assert_eq!(d.hci.peek32(0x483) & 0xff, 2);
    assert_eq!(d.hci.peek32(0x668), 1 << 7);
    set_channel(&mut d, 42, Bw::Mhz80, super::regs::RTW_SC_20_UPPER);
    assert_eq!(d.hci.peek32(0x3c60), 0x10100 | 0x1000 | 42);
    assert_eq!(d.hci.peek32(0x3c00 + 0x3f * 4), 0x8);
    // 80 MHz: TXSC20 upper (1), TXSC40 upper (9).
    assert_eq!(d.hci.peek32(0x483) & 0xff, 0x91);
    assert_eq!(d.hci.peek32(0x668), 1 << 8);
}
