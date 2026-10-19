//! Fuzz entry point, shared by the seeded tests and the cargo-fuzz target
//! (`fuzz/fuzz_targets/intelgfx.rs`), so a crash found by one replays under
//! the other.
//!
//! [`run`] reads a mode byte and feeds the rest to one parser (`0` EDID,
//! `1` ELD, `2` VBT, `3` HDMI N/CTS) or, for any other value, to all three
//! parsers. Nothing may panic, and whatever a parser accepts must keep its
//! promises (sizes inside the data, ELD round trip, N inside the spec range).

use crate::eld::{Eld, EldError};
use crate::vbt::Vbt;
use crate::{edid, hdmi_audio};

pub fn run(data: &[u8]) {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    match mode {
        0 => edid_bytes(rest),
        1 => eld_bytes(rest),
        2 => vbt_bytes(rest),
        3 => audio_bytes(rest),
        _ => {
            edid_bytes(rest);
            eld_bytes(rest);
            vbt_bytes(rest);
        }
    }
}

pub fn edid_bytes(data: &[u8]) {
    let Ok(e) = edid::Edid::parse(data) else {
        return;
    };
    for t in e
        .detailed
        .iter()
        .chain(e.cea.iter().flat_map(|c| &c.timings))
    {
        assert!(t.h_active > 0 && t.v_active > 0 && t.pixel_clock_khz > 0);
        let _ = t.refresh_millihz();
    }
    assert!(usize::from(e.bad_extensions) <= usize::from(e.extension_count));
    // Whatever the EDID says, building an ELD from it must work and parse.
    if let Some(eld) = Eld::from_edid(&e, crate::eld::ConnType::Hdmi, [0; 8]) {
        let bytes = eld.to_bytes();
        assert!(bytes.len() <= crate::eld::MAX_BYTES);
        let again = Eld::parse(&bytes).expect("a built ELD parses");
        assert_eq!(again.sads, eld.sads);
        assert_eq!(again.speakers, eld.speakers);
    }
}

pub fn eld_bytes(data: &[u8]) {
    match Eld::parse(data) {
        Ok(e) => {
            assert!(e.sads.len() <= 15);
            let bytes = e.to_bytes();
            assert_eq!(Eld::parse(&bytes), Ok(e.clone()));
            let _ = (e.lpcm_rates_hz(), e.max_lpcm_channels());
            let _ = e.supports_lpcm(48_000, 2);
        }
        Err(
            EldError::TooShort
            | EldError::BadVersion(_)
            | EldError::Truncated
            | EldError::BadLayout,
        ) => {}
    }
}

pub fn vbt_bytes(data: &[u8]) {
    let Ok(v) = Vbt::parse(data) else {
        return;
    };
    let mut total = 0usize;
    for (_, block) in v.blocks() {
        total += block.len() + 3;
        assert!(total <= data.len());
    }
    let children = v.children();
    assert!(children.len() <= 32);
    for c in &children {
        assert_ne!(c.device_type, 0);
        let _ = (c.can_hdmi(), crate::vbt::gmbus_pin(c.ddc_pin));
    }
}

pub fn audio_bytes(data: &[u8]) {
    if data.len() < 5 {
        return;
    }
    let khz = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let rate = hdmi_audio::Rate::ALL[usize::from(data[4]) % hdmi_audio::Rate::ALL.len()];
    if let Some(r) = hdmi_audio::select(khz, rate) {
        let (lo, hi) = hdmi_audio::n_range(rate);
        assert!((lo..=hi).contains(&r.n), "N {} outside {lo}..={hi}", r.n);
        if !r.from_table {
            assert_eq!(
                hdmi_audio::cts_for(khz, r.n, rate).map(|c| c.0),
                Some(r.cts)
            );
        }
    }
}
