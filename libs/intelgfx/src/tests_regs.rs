//! Tests of `regs`: offsets against the box's `intel_reg dump`, and the decoders.

use crate::regs::*;

#[test]
fn n_fields_round_trip() {
    for n in [4096, 6144, 6272, 11648, 24576, 0xFFFFF] {
        let c = aud_config::n_fields(n);
        assert_eq!(aud_config::n_value(c), n);
        assert_ne!(c & aud_config::N_PROG_ENABLE, 0);
    }
}

#[test]
fn ddi_func_decodes_hdmi_8bpc() {
    // Enabled, DDI B (select 1), HDMI mode, 8 bpc, both syncs positive.
    let v = 1 << 31 | 1 << 28 | 1 << 17 | 1 << 16;
    let f = DdiFunc::decode(v);
    assert!(f.enabled);
    assert_eq!(f.ddi_select, 1);
    assert_eq!(f.mode, DdiMode::Hdmi);
    assert_eq!(f.bits_per_component, Some(8));
    assert!(f.hsync_positive && f.vsync_positive);
    assert_eq!(DdiFunc::decode(1 << 24).mode, DdiMode::Dvi);
    assert_eq!(DdiFunc::decode(7 << 20).bits_per_component, None);
}

/// The survey's `intel_reg dump`, taken while i915 drove pipe A with
/// 2560x1440 at 70 Hz (277.82 MHz) on DDI B.
const DUMP: &str = include_str!("../../../docs/compat/kabylake/gpu/linux/01-intel_reg-dump.txt");

/// `(offset, value)` of the dump line `NAME (0xOFFSET): 0xVALUE ...`.
fn dump(name: &str) -> (u32, u32) {
    let hex = |s: &str| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap();
    for line in DUMP.lines() {
        let Some((head, tail)) = line.trim().split_once(" (0x") else {
            continue;
        };
        if head != name {
            continue;
        }
        let (offset, rest) = tail.split_once("): ").expect("dump line shape");
        let value = rest.split_whitespace().next().expect("dump value");
        return (hex(offset), hex(value));
    }
    panic!("{name} is not in the dump");
}

#[test]
fn confirmed_offsets_are_the_dumps() {
    for (name, offset) in [
        ("TRANS_HTOTAL_A", TRANS_A_BASE + trans::HTOTAL),
        ("TRANS_HBLANK_A", TRANS_A_BASE + trans::HBLANK),
        ("TRANS_HSYNC_A", TRANS_A_BASE + trans::HSYNC),
        ("TRANS_VTOTAL_A", TRANS_A_BASE + trans::VTOTAL),
        ("TRANS_VBLANK_A", TRANS_A_BASE + trans::VBLANK),
        ("TRANS_VSYNC_A", TRANS_A_BASE + trans::VSYNC),
        ("TRANS_DDI_FUNC_CTL_A", TRANS_DDI_FUNC_CTL_A),
        ("TRANS_DDI_FUNC_CTL_A", TRANS_A_BASE + trans::DDI_FUNC_CTL),
        ("TRANS_CONF_A", PIPE_A_BASE + PIPECONF),
        ("PLANE_CTL_1_A", PLANE_CTL_1_A),
        ("PLANE_STRIDE_1_A", PLANE_STRIDE_1_A),
        ("PLANE_POS_1_A", PLANE_POS_1_A),
        ("PLANE_SIZE_1_A", PLANE_SIZE_1_A),
        ("PLANE_SURF_1_A", PLANE_SURF_1_A),
        ("PLANE_BUF_CFG_1_A", PLANE_BUF_CFG_1_A),
        ("PLANE_WM_1_A_0", PLANE_WM_1_A_0),
        ("CDCLK_CTL", CDCLK_CTL),
        ("LCPLL1_CTL", LCPLL1_CTL),
        ("LCPLL2_CTL", LCPLL2_CTL),
        ("TRANS_CLK_SEL_A", TRANS_CLK_SEL_A),
        ("TRANS_CLK_SEL_B", TRANS_CLK_SEL_A + 4),
        ("DPLL_CTRL1", DPLL_CTRL1),
        ("DPLL_CTRL2", DPLL_CTRL2),
        ("DPLL_STATUS", DPLL_STATUS),
        ("DPLL1_CFGCR1", DPLL1_CFGCR1),
        ("DPLL1_CFGCR2", DPLL1_CFGCR2),
        ("DPLL2_CFGCR1", DPLL1_CFGCR1 + DPLL_CFGCR_STRIDE),
        ("DPLL3_CFGCR1", DPLL1_CFGCR1 + 2 * DPLL_CFGCR_STRIDE),
        ("WM_LINETIME_A", WM_LINETIME_A),
        ("VIDEO_DIP_CTL_A", VIDEO_DIP_CTL_A),
        ("PWR_WELL_BIOS", PWR_WELL_CTL_BIOS),
        ("PWR_WELL_DRIVER", PWR_WELL_CTL_DRIVER),
        ("DC_STATE_EN", DC_STATE_EN),
    ] {
        assert_eq!(dump(name).0, offset, "{name}");
    }
    // Per-pipe stride: plane 1 of pipe B is one pipe block up.
    assert_eq!(dump("PLANE_CTL_1_B").0, PLANE_CTL_1_A + PIPE_STRIDE);
    assert_eq!(dump("TRANS_HTOTAL_B").0, TRANS_A_BASE + TRANS_STRIDE);
}

#[test]
fn decodes_the_boxs_display_state() {
    let v = |name| dump(name).1;
    assert_eq!(cdclk_khz(v("CDCLK_CTL")), 337_500);
    assert_eq!(trans_clk_sel_port(v("TRANS_CLK_SEL_A")), Some(1), "DDI B");
    assert_eq!(trans_clk_sel_port(v("TRANS_CLK_SEL_B")), None);
    // PLL1 is in HDMI mode; PLL0 (the CDCLK PLL) is not.
    assert!(dpll_ctrl1_hdmi_mode(v("DPLL_CTRL1"), 1));
    assert!(!dpll_ctrl1_hdmi_mode(v("DPLL_CTRL1"), 0));
    // DDI B (port 1) is overridden to PLL1; DDI A has no override.
    assert_eq!(dpll_ctrl2_ddi_pll(v("DPLL_CTRL2"), 1), Some(1));
    assert_eq!(dpll_ctrl2_ddi_pll(v("DPLL_CTRL2"), 0), None);
    let f = DdiFunc::decode(v("TRANS_DDI_FUNC_CTL_A"));
    assert!(f.enabled);
    assert_eq!(f.ddi_select, 1);
    assert_eq!(f.mode, DdiMode::Hdmi, "HDMI mode: audio needs no mode set");
    assert_eq!(f.bits_per_component, Some(8));
    assert!(f.hsync_positive && f.vsync_positive);
    assert!(!DdiFunc::decode(v("TRANS_DDI_FUNC_CTL_B")).enabled);
    assert_eq!(timing_pair(v("TRANS_HTOTAL_A")), (2560, 2670));
    assert_eq!(timing_pair(v("TRANS_VTOTAL_A")), (1440, 1488));
    assert_eq!(plane_size(v("PLANE_SIZE_1_A")), (2560, 1440));
    let pipeconf = v("TRANS_CONF_A");
    assert_ne!(pipeconf & PIPECONF_ENABLE, 0);
    assert_ne!(pipeconf & PIPECONF_STATE, 0);
    assert_ne!(v("PLANE_CTL_1_A") & PLANE_CTL_ENABLE, 0);
    for bit in [dip::ENABLE_AVI, dip::ENABLE_GCP, dip::ENABLE_SPD] {
        assert_ne!(v("VIDEO_DIP_CTL_A") & bit, 0);
    }
}

#[test]
fn new_offsets_follow_the_documented_strides() {
    assert_eq!(DPLL1_CFGCR2, DPLL1_CFGCR1 + 4);
}

#[test]
fn pipe_and_transcoder_addresses_follow_the_strides() {
    assert_eq!(PIPE_A_BASE + PIPECONF, 0x70008);
    assert_eq!(TRANS_A_BASE + trans::DDI_FUNC_CTL, TRANS_DDI_FUNC_CTL_A);
    assert_eq!(PLANE_CTL_1_A + 0xC0, PLANE_WM_1_A_0);
    assert_eq!(pipesrc_size(2559 << 16 | 1439), (2560, 1440));
    assert_eq!(TRANS_A_BASE + TRANS_PIPESRC, 0x6001C);
    assert_eq!(timing_pair(2719 << 16 | 2559), (2560, 2720));
}

#[test]
fn dpll_clock_and_plane_decode_match_the_boxs_dump() {
    // DPLL1 as Linux left it: 277.82 MHz.
    assert_eq!(dpll_hdmi_clock_khz(0x8046_675B, 0x147), Some(277_820));
    assert_eq!(dpll_hdmi_clock_khz(0x0046_675B, 0x147), None, "DCO off");
    assert_eq!(
        dpll_hdmi_clock_khz(0x8046_675B, 3 << 2),
        None,
        "reserved P0"
    );
    // PLANE_CTL_1_A 0xc404b020: enabled, XRGB 8888, Y tiled (stride 0x50
    // x 128 = 10240 = 2560 x 4).
    let ctl = 0xC404_B020;
    assert_ne!(ctl & plane_ctl::ENABLE, 0);
    assert_eq!(plane_ctl::format(ctl), plane_ctl::Format::Xrgb8888);
    assert_eq!(plane_ctl::tiling(ctl), plane_ctl::Tiling::Y);
    assert_eq!(0x50 * plane_ctl::tiling(ctl).stride_unit(), 2560 * 4);
    assert_eq!(plane_ctl::tiling(0), plane_ctl::Tiling::Linear);
    // The power wells: both display wells state bits are up in the dump.
    for v in [0x5000_0011, 0xF000_0033] {
        assert_ne!(v & pwr_well::PW1_STATE, 0);
        assert_ne!(v & pwr_well::PW2_STATE, 0);
    }
    assert_eq!(pipe_base(1), 0x71000);
    assert_eq!(trans_base(2), 0x62000);
    assert_eq!(trans_clk_sel(1), 0x46144);
}
