//! ELD parsing, building from an EDID, and the byte layout the HDA spec
//! gives (a hand-written ELD, not one made by the builder).

use crate::build;
use crate::cea::Sad;
use crate::edid::Edid;
use crate::eld::{ConnType, Eld, EldError};

/// An ELD written out byte by byte from the layout table in `eld.rs`:
/// version 2, baseline 24 bytes, CEA 3, name "TEST", one LPCM SAD, HDMI.
fn literal() -> std::vec::Vec<u8> {
    let mut e = std::vec![0u8; 28];
    e[0] = 0x10; // ELD_Ver 2 << 3
    e[2] = 6; // baseline length: 6 x 4 bytes
    e[4] = 3 << 5 | 4; // CEA EDID 3, MNL 4
    e[5] = 1 << 4; // one SAD, connection type HDMI
    e[6] = 10; // 20 ms
    e[7] = 0x01; // FL/FR
    e[8] = 0x11; // port id
    e[16..18].copy_from_slice(&[0x1A, 0x45]);
    e[18..20].copy_from_slice(&[0x34, 0x12]);
    e[20..24].copy_from_slice(b"TEST");
    e[24..27].copy_from_slice(&[0x09, 0x04, 0x01]); // LPCM 2ch 48 kHz 16 bit
    e
}

#[test]
fn parses_the_documented_layout() {
    let e = Eld::parse(&literal()).unwrap();
    assert_eq!(e.version, 2);
    assert!(!e.is_partial());
    assert_eq!(e.cea_edid_version, 3);
    assert_eq!(e.monitor_name, "TEST");
    assert_eq!(e.conn_type, ConnType::Hdmi);
    assert_eq!(e.audio_sync_delay_ms(), 20);
    assert_eq!(e.speakers, 0x01);
    assert_eq!(e.port_id[0], 0x11);
    assert_eq!(e.manufacturer, [0x1A, 0x45]);
    assert_eq!(e.product_code, 0x1234);
    assert_eq!(e.sads.len(), 1);
    assert_eq!(e.lpcm_rates_hz(), std::vec![48_000]);
    assert_eq!(e.max_lpcm_channels(), 2);
    assert!(e.supports_lpcm(48_000, 2));
    assert!(!e.supports_lpcm(48_000, 6));
    assert!(!e.supports_lpcm(44_100, 2));
    // Writing it back gives the same bytes.
    assert_eq!(e.to_bytes(), literal());
}

#[test]
fn built_from_the_qhd_monitor() {
    let edid = Edid::parse(&build::monitor_2560x1440()).unwrap();
    let eld = Eld::from_edid(&edid, ConnType::Hdmi, [0xA5; 8]).unwrap();
    assert_eq!(eld.version, 2);
    assert_eq!(eld.cea_edid_version, 3);
    assert_eq!(eld.monitor_name, "LZY QHD27");
    assert_eq!(eld.sads.len(), 2);
    assert_eq!(eld.speakers, 0x01);
    // Latency byte 21 means (21 - 1) * 2 = 40 ms: ELD stores it in 2 ms units.
    assert_eq!(eld.audio_sync_delay_ms(), 40);
    assert_eq!(eld.lpcm_rates_hz(), std::vec![32_000, 44_100, 48_000]);
    assert!(eld.supports_lpcm(48_000, 2));
    assert!(!eld.supports_lpcm(96_000, 2));

    let bytes = eld.to_bytes();
    // 4 + ceil((16 + 9 + 6) / 4) * 4 = 36 bytes; baseline length 8.
    assert_eq!(bytes.len(), 36);
    assert_eq!(bytes[0], 0x10);
    assert_eq!(bytes[2], 8);
    assert_eq!(bytes[4], 3 << 5 | 9);
    assert_eq!(bytes[5], 2 << 4);
    assert_eq!(&bytes[20..29], b"LZY QHD27");
    // The manufacturer is the EDID's own two bytes: "LZY" = 12, 26, 25.
    assert_eq!(&bytes[16..18], &(12u16 << 10 | 26 << 5 | 25).to_be_bytes());
    assert_eq!(Eld::parse(&bytes), Ok(eld));
}

#[test]
fn no_cea_means_no_eld() {
    let edid = Edid::parse(&build::dvi_monitor_1080p()).unwrap();
    assert!(Eld::from_edid(&edid, ConnType::Hdmi, [0; 8]).is_none());
}

#[test]
fn display_port_and_partial_versions() {
    let mut e = Eld::parse(&literal()).unwrap();
    e.conn_type = ConnType::DisplayPort;
    e.supports_ai = true;
    e.version = crate::eld::VER_PARTIAL;
    let again = Eld::parse(&e.to_bytes()).unwrap();
    assert_eq!(again.conn_type, ConnType::DisplayPort);
    assert!(again.supports_ai && again.is_partial());
}

#[test]
fn refuses_what_it_cannot_trust() {
    assert_eq!(Eld::parse(&[]), Err(EldError::TooShort));
    assert_eq!(Eld::parse(&literal()[..19]), Err(EldError::TooShort));

    // An invalid ELD reads as version 0 (all zeros from the codec).
    assert_eq!(Eld::parse(&[0u8; 64]), Err(EldError::BadVersion(0)));

    let mut e = literal();
    e[0] = 1 << 3;
    assert_eq!(Eld::parse(&e), Err(EldError::BadVersion(1)));

    // Baseline longer than the data, and longer than 256 bytes.
    let mut e = literal();
    e[2] = 7;
    assert_eq!(Eld::parse(&e), Err(EldError::Truncated));
    let mut e = std::vec![0u8; 300];
    e[0] = 0x10;
    e[2] = 70;
    assert_eq!(Eld::parse(&e), Err(EldError::Truncated));

    // Monitor name or SAD list that does not fit the baseline.
    let mut e = literal();
    e[4] = 3 << 5 | 20;
    assert_eq!(Eld::parse(&e), Err(EldError::BadLayout));
    let mut e = literal();
    e[5] = 15 << 4;
    assert_eq!(Eld::parse(&e), Err(EldError::BadLayout));
}

#[test]
fn builder_caps_names_and_descriptors() {
    let mut eld = Eld::parse(&literal()).unwrap();
    eld.monitor_name = "A VERY LONG MONITOR NAME INDEED".into();
    eld.sads = std::vec![Sad::from_bytes([0x09, 0x07, 0x07]); 20];
    let bytes = eld.to_bytes();
    assert!(bytes.len() <= crate::eld::MAX_BYTES);
    let back = Eld::parse(&bytes).unwrap();
    assert_eq!(back.monitor_name.len(), 16);
    assert_eq!(back.sads.len(), 15);
}

#[test]
fn seeded_fuzz() {
    let seed = literal();
    let built = {
        let edid = Edid::parse(&build::monitor_2560x1440()).unwrap();
        Eld::from_edid(&edid, ConnType::Hdmi, [0; 8])
            .unwrap()
            .to_bytes()
    };
    fuzzkit::for_seeds("intelgfx_eld_mutated", |_, rng| {
        let mut data = if rng.one_in(2) {
            seed.clone()
        } else {
            built.clone()
        };
        let flips = rng.range(1, 8) as usize;
        rng.flip_bits(&mut data, flips);
        if rng.one_in(4) {
            data.truncate(rng.below(data.len() as u64 + 1) as usize);
        }
        crate::fuzz::eld_bytes(&data);
    });
    fuzzkit::for_seeds("intelgfx_eld_random", |_, rng| {
        let n = rng.range(0, 280) as usize;
        let mut data = rng.bytes(n);
        if !data.is_empty() && rng.one_in(2) {
            data[0] = 0x10;
        }
        crate::fuzz::eld_bytes(&data);
        crate::fuzz::audio_bytes(&data);
    });
}
