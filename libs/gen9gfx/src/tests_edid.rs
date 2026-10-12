//! EDID and CEA-861 extension tests: the 2560x1440 audio monitor fixture,
//! malformed input, and a seeded fuzz of the shared entry point.

use crate::build::{self, BaseSpec, CeaSpec};
use crate::cea;
use crate::edid::{checksum, Edid, EdidError, Timing, BLOCK};

#[test]
fn qhd_monitor_with_audio() {
    let bytes = build::monitor_2560x1440();
    assert_eq!(bytes.len(), 2 * BLOCK);
    let e = Edid::parse(&bytes).unwrap();
    assert_eq!(e.manufacturer_str(), "LZY");
    assert_eq!((e.version, e.revision), (1, 4));
    assert!(e.digital);
    assert_eq!(e.year, 2023);
    assert_eq!(e.product_code, 0x2560);
    assert_eq!(e.monitor_name.as_deref(), Some("LZY QHD27"));
    assert_eq!(e.monitor_serial, None);

    let p = e.preferred().unwrap();
    assert_eq!((p.h_active, p.v_active), (2560, 1440));
    assert_eq!(p.pixel_clock_khz, 241_500);
    assert_eq!((p.h_total(), p.v_total()), (2720, 1481));
    // 241.5 MHz / (2720 * 1481) = 59.95 Hz.
    assert_eq!(p.refresh_millihz() / 10, 5995);
    assert!(p.h_sync_positive && !p.v_sync_positive);
    assert_eq!((p.h_size_mm, p.v_size_mm), (597, 336));
    assert_eq!(e.detailed.len(), 2);
    assert_eq!(e.detailed[1].h_active, 1920);

    // The standard timing 0x81 0x80 is 1280x1024 at 60 Hz (5:4).
    assert_eq!(e.standard.len(), 1);
    assert_eq!(
        (e.standard[0].h_active, e.standard[0].v_active, e.standard[0].refresh_hz),
        (1280, 1024, 60)
    );
    let r = e.range.unwrap();
    assert_eq!((r.v_min_hz, r.v_max_hz, r.h_min_khz, r.h_max_khz), (48, 75, 30, 90));
    assert_eq!(r.max_pixel_clock_mhz, 300);

    assert_eq!((e.extension_count, e.bad_extensions), (1, 0));
    assert!(e.is_hdmi() && e.has_audio());
    let c = e.cea.as_ref().unwrap();
    assert_eq!(c.revision, 3);
    assert!(c.underscan && c.basic_audio && !c.malformed);
    assert_eq!(c.speakers, Some(0x01));
    assert_eq!(c.sads.len(), 2);
    assert!(c.sads[0].is_lpcm());
    assert_eq!(c.sads[0].channels, 2);
    assert!(c.sads[0].supports_rate(32_000));
    assert!(c.sads[0].supports_rate(44_100));
    assert!(c.sads[0].supports_rate(48_000));
    assert!(!c.sads[0].supports_rate(96_000));
    assert_eq!(c.sads[0].lpcm_depths(), 7);
    assert_eq!(c.sads[1].format, cea::format::AC3);
    assert_eq!(c.sads[1].channels, 6);
    assert_eq!(c.sads[1].detail, 0x50);
    assert_eq!(c.max_lpcm_channels(), 2);

    // 0x90 is VIC 16 with the native flag; 4, 31, 32 are plain; 0x85 is VIC 5
    // (native flag set).
    let vics: std::vec::Vec<(u8, bool)> = c.svds.iter().map(|s| (s.vic, s.native)).collect();
    assert_eq!(
        vics,
        [(16, true), (4, false), (31, false), (32, false), (5, true)]
    );
    assert_eq!(cea::vic_format(16), Some((1920, 1080, 60, false)));
    assert_eq!(cea::vic_format(5), Some((1920, 1080, 60, true)));

    let h = c.hdmi.unwrap();
    assert_eq!(h.phys_addr, 0x1000);
    assert!(h.dc_30bit && h.dc_36bit && h.dc_y444 && !h.dc_48bit && !h.supports_ai);
    assert_eq!(h.max_tmds_mhz, Some(300));
    assert_eq!(h.audio_latency, Some(21));
    assert_eq!(h.audio_latency_ms(), Some(40));
    // 241.5 MHz fits this sink's TMDS limit.
    assert!(u32::from(h.max_tmds_mhz.unwrap()) * 1000 >= p.pixel_clock_khz);
}

#[test]
fn dvi_monitor_has_no_audio() {
    let e = Edid::parse(&build::dvi_monitor_1080p()).unwrap();
    assert!(e.cea.is_none());
    assert!(!e.is_hdmi() && !e.has_audio());
    assert_eq!(e.preferred().unwrap().pixel_clock_khz, 148_500);
}

#[test]
fn cea_extension_variants() {
    // HDMI Forum block, extension DTDs, no audio descriptors.
    let base = build::base_block(&BaseSpec {
        manufacturer: *b"UHD",
        product: 7,
        serial: 1,
        timings: &[build::timing_1080p60()],
        name: None,
        monitor_serial: Some("SN0000001"),
        extensions: 1,
    });
    let ext = build::cea_block(&CeaSpec {
        svds: std::vec![97, 0xE0],
        hdmi: Some((0x2000, 0x00, 0xFF, None)),
        forum: Some((1, 120, 0x80)),
        dtds: std::vec![build::timing_2560x1440()],
        ..CeaSpec::default()
    });
    let e = Edid::parse(&[base.as_slice(), ext.as_slice()].concat()).unwrap();
    assert_eq!(e.monitor_serial.as_deref(), Some("SN0000001"));
    assert!(e.range.is_some());
    let c = e.cea.as_ref().unwrap();
    assert_eq!(c.timings.len(), 1);
    assert_eq!(c.timings[0].h_active, 2560);
    // VIC 97 has no native flag; 0xE0 is 224 (not in 129..=192): VIC 224.
    assert_eq!((c.svds[0].vic, c.svds[0].native), (97, false));
    assert_eq!((c.svds[1].vic, c.svds[1].native), (0xE0, false));
    let f = c.hdmi_forum.unwrap();
    assert_eq!((f.version, f.max_tmds_mhz, f.scdc_present), (1, 600, true));
    assert_eq!(c.hdmi.unwrap().max_tmds_mhz, Some(1275));
    assert!(c.hdmi.unwrap().audio_latency.is_none());
    // An HDMI sink without audio blocks and without basic audio.
    assert!(e.is_hdmi());
    assert!(!e.has_audio());
}

#[test]
fn rejects_malformed_base_blocks() {
    let good = build::monitor_2560x1440();
    assert_eq!(Edid::parse(&[]), Err(EdidError::TooShort));
    assert_eq!(Edid::parse(&good[..127]), Err(EdidError::TooShort));

    let mut bad = good.clone();
    bad[0] = 1;
    assert_eq!(Edid::parse(&bad), Err(EdidError::BadHeader));

    let mut bad = good.clone();
    bad[100] ^= 0x40;
    assert_eq!(Edid::parse(&bad), Err(EdidError::BadChecksum));

    let mut bad = good.clone();
    bad[18] = 2;
    crate::build::seal(&mut bad[..BLOCK]);
    assert_eq!(Edid::parse(&bad), Err(EdidError::BadVersion));

    assert_eq!(checksum(&good[..BLOCK]), 0);
    assert_eq!(checksum(&good[BLOCK..]), 0);
}

#[test]
fn bad_extensions_are_counted_not_fatal() {
    let good = build::monitor_2560x1440();
    // Checksum broken.
    let mut bad = good.clone();
    bad[BLOCK + 50] ^= 1;
    let e = Edid::parse(&bad).unwrap();
    assert_eq!((e.extension_count, e.bad_extensions), (1, 1));
    assert!(e.cea.is_none());
    assert_eq!(e.preferred().unwrap().h_active, 2560);

    // Extension announced but not delivered.
    let e = Edid::parse(&good[..BLOCK]).unwrap();
    assert_eq!(e.bad_extensions, 1);

    // A CEA block with a reserved revision.
    let mut bad = good.clone();
    bad[BLOCK + 1] = 0;
    crate::build::seal(&mut bad[BLOCK..]);
    assert_eq!(Edid::parse(&bad).unwrap().bad_extensions, 1);

    // A data block that runs past the DTD offset: kept so far, flagged.
    let mut bad = good.clone();
    bad[BLOCK + 4] = 2 << 5 | 31;
    crate::build::seal(&mut bad[BLOCK..]);
    let e = Edid::parse(&bad).unwrap();
    assert!(e.cea.unwrap().malformed);
}

#[test]
fn timing_encoding_round_trips_wide_fields() {
    let t = Timing {
        pixel_clock_khz: 594_000,
        h_active: 3840,
        h_blank: 560,
        h_front: 176,
        h_sync: 88,
        v_active: 2160,
        v_blank: 90,
        v_front: 8,
        v_sync: 10,
        h_size_mm: 1210,
        v_size_mm: 680,
        interlaced: false,
        h_sync_positive: true,
        v_sync_positive: true,
    };
    assert_eq!(Timing::parse(&build::dtd(&t)), Some(t));
    // Fields that need the high bits: 10-bit front porch, 6-bit vsync.
    let wide = Timing {
        h_front: 700,
        h_sync: 300,
        v_front: 50,
        v_sync: 40,
        interlaced: true,
        h_sync_positive: false,
        v_sync_positive: false,
        ..t
    };
    assert_eq!(Timing::parse(&build::dtd(&wide)), Some(wide));
    // A display descriptor is not a timing; neither is an empty one.
    assert_eq!(Timing::parse(&[0; 18]), None);
    assert_eq!(Timing::parse(&build::dtd(&t)[..17]), None);
}

#[test]
fn seeded_fuzz() {
    use crate::fuzz;
    let seeds = [
        build::monitor_2560x1440(),
        build::dvi_monitor_1080p(),
    ];
    fuzzkit::for_seeds("gen9gfx_edid_mutated", |_, rng| {
        let mut data = rng.pick(&seeds).clone();
        let flips = rng.range(1, 12) as usize;
        rng.flip_bits(&mut data, flips);
        if rng.one_in(4) {
            data.truncate(rng.below(data.len() as u64 + 1) as usize);
        }
        // Mutations rarely keep the checksum, so also try them re-sealed.
        fuzz::edid_bytes(&data);
        for block in data.chunks_exact_mut(BLOCK) {
            crate::build::seal(block);
        }
        fuzz::edid_bytes(&data);
        let mut framed = std::vec![0u8];
        framed.extend_from_slice(&data);
        fuzz::run(&framed);
    });
    fuzzkit::for_seeds("gen9gfx_edid_random", |_, rng| {
        let n = rng.range(0, 300) as usize;
        let mut data = rng.bytes(n);
        // Make a block pass the cheap gates half the time.
        if data.len() >= BLOCK && rng.one_in(2) {
            data[..8].copy_from_slice(&crate::edid::HEADER);
            data[18] = 1;
            crate::build::seal(&mut data[..BLOCK]);
        }
        fuzz::edid_bytes(&data);
    });
}
