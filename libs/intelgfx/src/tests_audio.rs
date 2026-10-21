//! `audio` against a register file that enforces the ordering rules of the
//! sequence (the "sequence fake" of plan section 6): a rule broken is a
//! recorded violation, so a reordering of `audio::enable` fails here.

use std::collections::BTreeMap;
use std::vec::Vec;

use crate::audio::{self, Error, Request};
use crate::hdmi_audio::{aud_config_clock_index, select, tmds_clock_khz, Rate};
use crate::regs::{
    aud_config, AUD_CONFIG_A, AUD_DIP_ELD_CTRL_ST_A, AUD_HDMIW_HDMIEDID_A, AUD_PIN_ELD_CP_VLD,
    TRANS_DDI_FUNC_CTL_A,
};
use crate::Mmio;

/// The box's firmware state: transcoder A enabled on DDI B in HDMI mode.
const FUNC_HDMI: u32 = 0x9001_0000;
/// The same with the link in DVI mode (mode field 1).
const FUNC_DVI: u32 = 0x9101_0000;

#[derive(Default)]
struct Engine {
    regs: BTreeMap<u32, u32>,
    eld: Vec<u32>,
    violations: Vec<&'static str>,
    log: Vec<(u32, u32)>,
}

impl Engine {
    fn with_link(func: u32) -> Engine {
        let mut e = Engine::default();
        e.regs.insert(TRANS_DDI_FUNC_CTL_A, func);
        e
    }

    fn valid(&self) -> bool {
        self.regs.get(&AUD_PIN_ELD_CP_VLD).copied().unwrap_or(0) & 1 != 0
    }
}

impl Mmio for Engine {
    fn read32(&mut self, offset: u32) -> u32 {
        self.regs.get(&offset).copied().unwrap_or(0)
    }

    fn write32(&mut self, offset: u32, value: u32) {
        self.log.push((offset, value));
        if offset == AUD_HDMIW_HDMIEDID_A {
            if self.valid() {
                self.violations.push("ELD written while valid");
            }
            if self.regs.get(&AUD_PIN_ELD_CP_VLD).copied().unwrap_or(0) & 4 == 0 {
                self.violations.push("ELD written with output disabled");
            }
            self.eld.push(value);
            return;
        }
        if offset == AUD_DIP_ELD_CTRL_ST_A {
            self.eld.clear(); // the address reset restarts the buffer
        }
        if offset == AUD_PIN_ELD_CP_VLD && value & 1 != 0 && self.eld.is_empty() {
            self.violations.push("valid with an empty ELD buffer");
        }
        self.regs.insert(offset, value);
    }
}

fn eld() -> Vec<u8> {
    (0u8..40).collect()
}

fn request(eld: &[u8], tmds_khz: u32, n: Option<u32>) -> Request<'_> {
    Request {
        transcoder: 0,
        eld,
        tmds_khz,
        n,
    }
}

#[test]
fn enable_follows_the_sequence_and_the_buffer_holds_the_eld() {
    let mut e = Engine::with_link(FUNC_HDMI);
    let bytes = eld();
    let tmds = tmds_clock_khz(241_500, 8);
    let n = select(tmds, Rate::Hz48000).unwrap().n;
    let done = audio::enable(&mut e, &request(&bytes, tmds, Some(n))).unwrap();
    assert!(e.violations.is_empty(), "{:?}", e.violations);
    assert_eq!(done.eld_dwords, 10);
    let words: Vec<u32> = bytes
        .chunks(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    assert_eq!(e.eld, words);
    assert_eq!(
        e.regs[&AUD_PIN_ELD_CP_VLD] & 0b101,
        0b101,
        "valid and enabled"
    );
    assert_eq!(aud_config::n_value(e.regs[&AUD_CONFIG_A]), 6144);
    assert_ne!(e.regs[&AUD_CONFIG_A] & aud_config::N_PROG_ENABLE, 0);
}

#[test]
fn a_clock_with_a_table_index_programs_the_index_not_n() {
    let mut e = Engine::with_link(FUNC_HDMI);
    let bytes = eld();
    audio::enable(&mut e, &request(&bytes, 148_500, None)).unwrap();
    let cfg = e.regs[&AUD_CONFIG_A];
    assert_eq!(
        (cfg & aud_config::CLOCK_MASK) >> aud_config::CLOCK_SHIFT,
        aud_config_clock_index(148_500).unwrap()
    );
    assert_eq!(cfg & aud_config::N_PROG_ENABLE, 0);
}

#[test]
fn the_other_aud_config_bits_survive() {
    let mut e = Engine::with_link(FUNC_HDMI);
    e.regs
        .insert(AUD_CONFIG_A, 0x0000_0008 | aud_config::N_VALUE_INDEX);
    let bytes = eld();
    audio::enable(&mut e, &request(&bytes, 148_500, None)).unwrap();
    let cfg = e.regs[&AUD_CONFIG_A];
    assert_eq!(cfg & 0x8, 0x8, "DISABLE_NCTS is not ours to clear");
    assert_eq!(
        cfg & aud_config::N_VALUE_INDEX,
        0,
        "that bit is the DP form"
    );
}

#[test]
fn a_dvi_link_is_refused_and_nothing_is_written() {
    let mut e = Engine::with_link(FUNC_DVI);
    let bytes = eld();
    let err = audio::enable(&mut e, &request(&bytes, 241_500, Some(6144))).unwrap_err();
    assert!(matches!(err, Error::NotHdmi(_)));
    assert!(e.log.is_empty());
}

#[test]
fn an_idle_transcoder_is_refused() {
    let mut e = Engine::with_link(0);
    let bytes = eld();
    let err = audio::enable(&mut e, &request(&bytes, 241_500, Some(6144))).unwrap_err();
    assert!(matches!(err, Error::TranscoderOff(0)));
    assert!(e.log.is_empty());
}

#[test]
fn bad_requests_write_nothing() {
    let mut e = Engine::with_link(FUNC_HDMI);
    let long = [0u8; 88];
    for (eld, tmds, n, want) in [
        (&long[..], 148_500, None, Error::BadEld(88)),
        (&long[..0], 148_500, None, Error::BadEld(0)),
        (&long[..6], 148_500, None, Error::BadEld(6)),
        (&long[..40], 241_500, None, Error::NoClockIndex(241_500)),
    ] {
        assert_eq!(audio::enable(&mut e, &request(eld, tmds, n)), Err(want));
    }
    let mut r = request(&long[..40], 148_500, None);
    r.transcoder = 3;
    assert_eq!(audio::enable(&mut e, &r), Err(Error::TransOutOfRange(3)));
    assert!(e.log.is_empty());
}

#[test]
fn the_longest_eld_fits_and_disable_undoes_enable() {
    let mut e = Engine::with_link(FUNC_HDMI);
    let long = [0x5Au8; 84];
    audio::enable(&mut e, &request(&long, 148_500, None)).unwrap();
    assert_eq!(e.eld.len(), 21);
    audio::disable(&mut e, 0).unwrap();
    assert_eq!(e.regs[&AUD_PIN_ELD_CP_VLD] & 0b101, 0);
    audio::disable(&mut e, 0).unwrap();
    assert!(e.violations.is_empty());
}

#[test]
fn repeated_enable_and_disable_never_breaks_a_rule() {
    // The soak: a monitor power cycle a thousand times over.
    let mut e = Engine::with_link(FUNC_HDMI);
    let bytes = eld();
    for i in 0..1000u32 {
        let rate = [Rate::Hz32000, Rate::Hz44100, Rate::Hz48000][(i % 3) as usize];
        let tmds = tmds_clock_khz(241_500, 8);
        let n = select(tmds, rate).unwrap().n;
        audio::enable(&mut e, &request(&bytes, tmds, Some(n))).unwrap();
        audio::set_clock(&mut e, 0, tmds, Some(n)).unwrap();
        audio::disable(&mut e, 0).unwrap();
    }
    assert!(e.violations.is_empty(), "{:?}", e.violations);
}

#[test]
fn other_transcoders_use_their_own_nibble_and_block() {
    let mut e = Engine::with_link(FUNC_HDMI);
    e.regs.insert(TRANS_DDI_FUNC_CTL_A + 0x1000, FUNC_HDMI);
    let bytes = eld();
    let mut r = request(&bytes, 148_500, None);
    r.transcoder = 1;
    audio::enable(&mut e, &r).unwrap();
    assert_eq!(e.regs[&AUD_PIN_ELD_CP_VLD], 0b101 << 4);
    assert!(e.regs.contains_key(&(AUD_CONFIG_A + 0x100)));
    assert!(!e.regs.contains_key(&AUD_CONFIG_A));
}
