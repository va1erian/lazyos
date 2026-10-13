//! The parsers against what the Kaby Lake box really has: the EDID of its
//! monitor (serial redacted, checksum fixed) and the VBT and OpRegion i915
//! read from its firmware, from the G0 capture in
//! `docs/compat/kabylake/gpu/linux/` (`docs/kabylake-gpu-plan.md` section 2).
//!
//! The synthetic fixtures in `build.rs` test the code paths; these pin the
//! facts the plan's decisions rest on, so a parser change that would move one
//! shows up here before it shows up on the box.

use crate::edid::Edid;
use crate::eld::{ConnType, Eld};
use crate::hdmi_audio::{self, Rate};
use crate::vbt::{OutputKind, Port, Vbt};
use alloc::vec::Vec;

const EDID: &[u8] =
    include_bytes!("../../../docs/compat/kabylake/gpu/linux/04-card1-HDMI-A-1-edid.bin");
const VBT: &[u8] =
    include_bytes!("../../../docs/compat/kabylake/gpu/linux/03-dri0000_00_02.0-vbt.bin");
const OPREGION: &[u8] =
    include_bytes!("../../../docs/compat/kabylake/gpu/linux/03-dri0000_00_02.0-opregion.bin");

#[test]
fn opregion_carries_the_vbt_i915_exported() {
    // The OpRegion is the container `ASLS` (PCI config 0xFC) points at; its
    // mailbox 4 is the raw VBT, at +0x400, the same bytes as i915's `i915_vbt`.
    assert_eq!(&OPREGION[..16], b"IntelGraphicsMem");
    assert_eq!(OPREGION.len(), 8192);
    assert_eq!(&OPREGION[0x400..0x400 + VBT.len()], VBT);
    assert_eq!(&VBT[..4], b"$VBT");
}

#[test]
fn vbt_names_ddi_b_as_native_hdmi_without_lspcon() {
    let vbt = Vbt::parse(VBT).expect("the box's VBT parses");
    // BDB 209 (the SKL/KBL BIOS build of 2017). Its whole-table sum is not
    // zero, as with most firmware: `checksum_ok` is information, never a gate.
    assert_eq!(vbt.bdb_version, 209);
    assert!(!vbt.checksum_ok);
    let hdmi = vbt.hdmi_child().expect("an HDMI child device");
    // One HDMI output, on DDI B, DDC on GMBUS pin 5 (DPB), no AUX channel and
    // no LSPCON: DP AUX is not in scope for this board (plan section 2).
    assert_eq!(hdmi.port, Some(Port::B));
    assert_eq!(hdmi.kind, OutputKind::Hdmi);
    assert_eq!(hdmi.device_type, 0x60D2);
    assert_eq!(hdmi.ddc_pin, 5);
    assert_eq!(crate::vbt::gmbus_pin(hdmi.ddc_pin), Some(5));
    assert_eq!(hdmi.aux_channel, 0);
    assert!(!hdmi.lspcon);
    assert!(!hdmi.lane_reversal);
    assert!(hdmi.can_hdmi());
    assert!(!hdmi.dp_support);
}

#[test]
fn vbt_lists_a_second_hdmi_on_ddi_c_that_has_no_monitor() {
    let vbt = Vbt::parse(VBT).unwrap();
    let ports: Vec<_> = vbt
        .children()
        .iter()
        .filter(|c| c.kind == OutputKind::Hdmi)
        .map(|c| (c.port, c.ddc_pin))
        .collect();
    // Linux's i915 reports HDMI-A-2 disconnected, so only DDI B matters; the
    // VBT still describes DDI C (DDC pin 4).
    assert!(ports.contains(&(Some(Port::B), 5)), "{ports:?}");
    assert!(ports.contains(&(Some(Port::C), 4)), "{ports:?}");
}

#[test]
fn edid_is_a_hdmi_monitor_whose_preferred_mode_is_2560x1440_at_241_5_mhz() {
    let edid = Edid::parse(EDID).expect("the redacted EDID keeps valid checksums");
    assert_eq!(edid.bad_extensions, 0);
    assert_eq!(edid.manufacturer_str(), "IVM");
    assert_eq!(edid.monitor_name.as_deref(), Some("PL2730Q"));
    assert!(edid.digital);
    assert!(edid.is_hdmi());
    assert!(edid.has_audio());

    let pref = edid.preferred().expect("preferred timing");
    assert_eq!((pref.h_active, pref.v_active), (2560, 1440));
    assert_eq!(pref.pixel_clock_khz, 241_500);
    // 59.95 Hz, not 60.00: the CVT reduced-blanking timing.
    assert_eq!(pref.refresh_millihz() / 10, 5995);
}

#[test]
fn edid_caps_the_link_at_280_mhz_and_lists_a_277_82_mhz_2560x1440_mode() {
    let edid = Edid::parse(EDID).unwrap();
    let cea = edid.cea.as_ref().expect("CEA extension");
    // An HDMI 1.x sink: no HDMI Forum block, a 280 MHz TMDS ceiling. The 70 Hz
    // mode Linux ran is the fastest 1440p timing the EDID lists; it is
    // 2.18 MHz under that ceiling.
    assert_eq!(cea.hdmi.as_ref().and_then(|h| h.max_tmds_mhz), Some(280));
    assert!(cea.hdmi_forum.is_none());
    let seventy = cea
        .timings
        .iter()
        .find(|t| t.pixel_clock_khz == 277_820)
        .expect("the 2560x1440 70 Hz DTD");
    assert_eq!((seventy.h_active, seventy.v_active), (2560, 1440));
}

#[test]
fn edid_audio_is_two_channel_lpcm_only() {
    let edid = Edid::parse(EDID).unwrap();
    let cea = edid.cea.as_ref().unwrap();
    assert!(cea.basic_audio);
    assert_eq!(cea.sads.len(), 1);
    assert_eq!(cea.speakers, Some(0x01), "front left/right only");
}

#[test]
fn the_eld_built_from_the_edid_matches_what_the_codec_reported() {
    // 05-eld.txt: monitor PL2730Q, HDMI, CEA-861-B EDID, FL/FR, one SAD of
    // LPCM, 2 channels, 32/44.1/48 kHz, 16/20/24 bit.
    let edid = Edid::parse(EDID).unwrap();
    let eld = Eld::from_edid(&edid, ConnType::Hdmi, [0; 8]).expect("an ELD");
    assert_eq!(eld.monitor_name, "PL2730Q");
    assert_eq!(eld.speakers, 0x01);
    assert_eq!(eld.sads.len(), 1);
    assert_eq!(eld.max_lpcm_channels(), 2);
    assert_eq!(eld.lpcm_rates_hz(), [32_000, 44_100, 48_000]);
    assert!(eld.supports_lpcm(48_000, 2));
    assert!(!eld.supports_lpcm(96_000, 2));
    assert!(!eld.supports_lpcm(48_000, 8));
    // The byte form is exactly what the codec held: `amixer` `ELD` control of
    // PCM device 3 in 05-amixer.txt (32 bytes).
    const CODEC_ELD: [u8; 32] = [
        0x10, 0x00, 0x07, 0x00, 0x67, 0x10, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0x26, 0xcd, 0x43,
        0x66, b'P', b'L', b'2', b'7', b'3', b'0', b'Q', 0x09, 0x07, 0x07, 0x00, 0x00,
    ];
    assert_eq!(&eld.to_bytes()[..32], &CODEC_ELD);
    assert_eq!(Eld::parse(&CODEC_ELD).unwrap(), eld);
}

#[test]
fn hdmi_audio_clock_regeneration_for_both_modes_the_box_can_run() {
    // 8 bpc: the TMDS clock is the pixel clock.
    for pixel_khz in [241_500, 277_820] {
        let tmds = hdmi_audio::tmds_clock_khz(pixel_khz, 8);
        assert_eq!(tmds, pixel_khz);
        for rate in [Rate::Hz32000, Rate::Hz44100, Rate::Hz48000] {
            let n_cts = hdmi_audio::select(tmds, rate)
                .unwrap_or_else(|| panic!("N/CTS for {pixel_khz} kHz at {rate:?}"));
            // The clock regeneration relation 128 * fs = f_tmds * N / CTS.
            let lhs = 128 * u64::from(rate.hz()) * u64::from(n_cts.cts);
            let rhs = u64::from(tmds) * 1000 * u64::from(n_cts.n);
            let err = lhs.abs_diff(rhs) as f64 / rhs as f64;
            assert!(
                err < 1e-4,
                "{pixel_khz} kHz {rate:?}: N={} CTS={}",
                n_cts.n,
                n_cts.cts
            );
        }
    }
}
