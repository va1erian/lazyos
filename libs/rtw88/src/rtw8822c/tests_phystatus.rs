//! PHY status parsing on crafted buffers, and a seeded fuzz of the parser.

use super::phystatus::*;
use super::DmState;
use crate::dev::Bw;
use crate::phy::rate::{DESC_RATE54M, DESC_RATEMCS7};
use crate::rx::RxPktStat;

const CTX: PhyStatusCtx = PhyStatusCtx {
    cck_gi_l_bnd: 0x10,
    cck_gi_u_bnd: 0x30,
    current_bw: Bw::Mhz40,
};

#[test]
fn page0_cck_power_and_gain_correction() {
    let mut ps = [0u8; 32];
    ps[0] = 0x00; // page 0
    ps[1] = 60; // PWDB A
    ps[2] = 0x0c; // gain A below the window by 4 -> +8
    ps[6] = 6; // channel
    ps[16] = 55; // PWDB B
    ps[19] = 0x33; // gain B above the window by 3 -> -6
    let mut pkt = RxPktStat::default();
    query_phy_status(&CTX, &ps, &mut pkt).unwrap();
    assert_eq!(pkt.rx_power, [60 + 8 - 110, 55 - 6 - 110]);
    assert_eq!((pkt.band, pkt.freq), (0, 2437));
    assert_eq!(pkt.bw, Bw::Mhz20 as u8);
    assert_eq!(pkt.signal_power, -42);
    // One path (A) for the RSSI: -42 dBm -> 58.
    assert_eq!(pkt.rssi, 58);
    assert!(!pkt.channel_invalid);
}

#[test]
fn page0_channel_0_is_invalid_and_power_floors_at_minus_120() {
    let mut ps = [0u8; 20];
    ps[1] = 0; // 0 - 110 = -110, then the gain correction lowers it further
    ps[2] = 0x3f; // 0x3f - 0x30 = 15 -> -30
    let mut pkt = RxPktStat::default();
    query_phy_status(&CTX, &ps, &mut pkt).unwrap();
    assert!(pkt.channel_invalid);
    assert_eq!(pkt.rx_power[0], (-140i16) as i8); // s8 wrap, as upstream
    assert_eq!(pkt.signal_power, i32::from(((-140i16) as i8).max(-120)));
}

#[test]
fn page1_ofdm_fields() {
    let mut ps = [0u8; 32];
    ps[0] = 0x01;
    ps[1] = 70;
    ps[2] = 66;
    ps[5] = 0x9 << 4 | 0x2; // HT sub-channel 9 (40 MHz), legacy 2 (20 MHz)
    ps[6] = 149;
    ps[16] = 0xe0; // EVM A -32
    ps[17] = 0x05;
    ps[20] = 0xfc; // CFO tail A -4
    ps[21] = 0x03;
    ps[24] = 40; // SNR A
    ps[25] = 0xf6;
    let mut pkt = RxPktStat {
        rate: DESC_RATEMCS7,
        ..RxPktStat::default()
    };
    query_phy_status(&CTX, &ps, &mut pkt).unwrap();
    assert_eq!(pkt.rx_power, [-40, -44]);
    assert_eq!((pkt.band, pkt.freq), (1, 5745));
    assert_eq!(pkt.bw, Bw::Mhz40 as u8);
    assert_eq!(pkt.signal_power, -40);
    assert_eq!(pkt.rx_evm, [0xe0, 0x05]);
    assert_eq!(pkt.cfo_tail, [-4, 3]);
    assert_eq!(pkt.rx_snr, [40, -10]);
    // A legacy OFDM rate reads the low nibble: sub-channel 2 -> 20 MHz.
    let mut legacy = RxPktStat {
        rate: DESC_RATE54M,
        ..RxPktStat::default()
    };
    query_phy_status(&CTX, &ps, &mut legacy).unwrap();
    assert_eq!(legacy.bw, Bw::Mhz20 as u8);
    // Sub-channel 0 means the current width.
    ps[5] = 0;
    query_phy_status(&CTX, &ps, &mut pkt).unwrap();
    assert_eq!(pkt.bw, Bw::Mhz40 as u8);

    let mut dm = DmState::default();
    update_dm(&mut dm, 2, &pkt, true);
    assert_eq!(dm.rx_snr, [20, -5]);
    assert_eq!(dm.cfo_tail, [-10, 7]);
    // EVM A -32 -> 16 dB; B is positive and keeps A's value (upstream).
    assert_eq!(dm.rx_evm_dbm, [16, 16]);
    assert_eq!(dm.curr_rx_rate, DESC_RATEMCS7);
}

#[test]
fn short_or_unknown_buffers_are_refused_untouched() {
    let mut pkt = RxPktStat::default();
    assert!(query_phy_status(&CTX, &[], &mut pkt).is_err());
    assert!(query_phy_status(&CTX, &[0u8; PAGE0_LEN - 1], &mut pkt).is_err());
    let mut p1 = [0u8; PAGE1_LEN - 1];
    p1[0] = 1;
    assert!(query_phy_status(&CTX, &p1, &mut pkt).is_err());
    let mut p2 = [0u8; 32];
    p2[0] = 2;
    assert!(query_phy_status(&CTX, &p2, &mut pkt).is_err());
    assert_eq!(pkt, RxPktStat::default());
}

#[test]
fn fuzz_never_panics_and_stays_in_range() {
    fuzzkit::for_seeds("rtw8822c::query_phy_status", |_seed, rng| {
        let len = rng.below(40) as usize;
        let mut ps = std::vec![0u8; len];
        for b in ps.iter_mut() {
            *b = rng.byte();
        }
        if len > 0 && rng.one_in(2) {
            ps[0] &= 0xf1; // favour pages 0 and 1
        }
        let ctx = PhyStatusCtx {
            cck_gi_l_bnd: rng.byte() & 0x3f,
            cck_gi_u_bnd: rng.byte() & 0x3f,
            current_bw: [Bw::Mhz20, Bw::Mhz40, Bw::Mhz80][rng.below(3) as usize],
        };
        let mut pkt = RxPktStat {
            rate: rng.byte(),
            ..RxPktStat::default()
        };
        let before = pkt.clone();
        match query_phy_status(&ctx, &ps, &mut pkt) {
            Ok(()) => {
                assert!(pkt.signal_power >= -120 && pkt.signal_power <= 127);
                assert!(pkt.bw <= Bw::Mhz80 as u8);
                assert!(pkt.rssi <= 100);
                assert!(
                    pkt.freq == 0
                        || (2407..=2484).contains(&pkt.freq)
                        || (5180..=5885).contains(&pkt.freq)
                );
                let mut dm = DmState::default();
                update_dm(&mut dm, rng.below(4) as u8, &pkt, ps[0] & 0xf == 1);
            }
            Err(_) => assert_eq!(pkt, before),
        }
    });
}
