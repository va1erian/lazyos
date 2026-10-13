//! Gen9 display register offsets (BAR 0, bytes) for the plan's first stages:
//! G1 (adopt and observe the firmware's mode) and G2 (HDMI audio).
//!
//! Every constant here is from memory of i915's `i915_reg.h` and Intel's
//! Gen9 display PRM volumes and is **to confirm against a register dump from
//! the box**: `docs/compat/kabylake/gpu/` (`02-mmio-ranges.txt`) holds the
//! capture `tools/boot/survey_gpu.sh` makes, and `intel_reg dump` names the
//! same registers. Nothing here is written by any code yet; the offsets that
//! are laid out per pipe or per port use the strides i915 uses.
//! Marker: `// TO CONFIRM against dump` on each group.

// TO CONFIRM against dump -----------------------------------------------------

/// Pipe/transcoder A register block base, and the stride to the next pipe
/// (A 0x70000, B 0x71000, C 0x72000) for the pipe registers.
pub const PIPE_A_BASE: u32 = 0x7_0000;
pub const PIPE_STRIDE: u32 = 0x1000;
/// Transcoder timing registers (A 0x60000, B 0x61000, C 0x62000).
pub const TRANS_A_BASE: u32 = 0x6_0000;
pub const TRANS_STRIDE: u32 = 0x1000;

/// Offsets inside the transcoder block.
pub mod trans {
    pub const HTOTAL: u32 = 0x00;
    pub const HBLANK: u32 = 0x04;
    pub const HSYNC: u32 = 0x08;
    pub const VTOTAL: u32 = 0x0C;
    pub const VBLANK: u32 = 0x10;
    pub const VSYNC: u32 = 0x14;
    pub const PIPESRC: u32 = 0x1C;
    /// `TRANS_DDI_FUNC_CTL` sits at +0x400 of the transcoder block.
    pub const DDI_FUNC_CTL: u32 = 0x400;
}

/// `PIPECONF` offset inside the pipe block (`0x70008` for pipe A).
pub const PIPECONF: u32 = 0x08;
pub const PIPECONF_ENABLE: u32 = 1 << 31;
pub const PIPECONF_STATE: u32 = 1 << 30;

/// First universal plane's control, stride and surface registers for pipe A.
pub const PLANE_CTL_1_A: u32 = 0x7_0180;
pub const PLANE_STRIDE_1_A: u32 = 0x7_0188;
pub const PLANE_POS_1_A: u32 = 0x7_018C;
pub const PLANE_SIZE_1_A: u32 = 0x7_0190;
pub const PLANE_SURF_1_A: u32 = 0x7_019C;
pub const PLANE_CTL_ENABLE: u32 = 1 << 31;
/// Buffer allocation (DDB) and the first watermark level of plane 1, pipe A.
pub const PLANE_BUF_CFG_1_A: u32 = 0x7_027C;
pub const PLANE_WM_1_A_0: u32 = 0x7_0240;

/// Core display clock, LCPLLs, and the per-DDI clock select (A at 0x46100,
/// 4 bytes apart).
pub const CDCLK_CTL: u32 = 0x4_6000;
pub const LCPLL1_CTL: u32 = 0x4_6010;
pub const LCPLL2_CTL: u32 = 0x4_6014;
pub const PORT_CLK_SEL_A: u32 = 0x4_6100;

/// SKL-style shared DPLL control (DPLL1 to 3 share these) and status.
pub const DPLL_CTRL1: u32 = 0x6_C058;
pub const DPLL_CTRL2: u32 = 0x6_C05C;
pub const DPLL_STATUS: u32 = 0x6_C060;

/// DDI buffer control for DDI A, stride 0x100 per port; and the transcoder
/// function control of transcoder A.
pub const DDI_BUF_CTL_A: u32 = 0x6_4000;
pub const DDI_BUF_STRIDE: u32 = 0x100;
pub const DDI_BUF_CTL_ENABLE: u32 = 1 << 31;
pub const TRANS_DDI_FUNC_CTL_A: u32 = 0x6_0400;

/// Power wells and DC states.
pub const PWR_WELL_CTL_BIOS: u32 = 0x4_5400;
pub const PWR_WELL_CTL_DRIVER: u32 = 0x4_5404;
pub const DC_STATE_EN: u32 = 0x4_5504;

/// Audio: per-pipe registers at +0x100 per pipe from these (pipe A).
pub const AUD_CONFIG_A: u32 = 0x6_5000;
pub const AUD_MISC_CTRL_A: u32 = 0x6_5010;
pub const AUD_VID_DID: u32 = 0x6_5020;
pub const AUD_RATE_CAP: u32 = 0x6_5028;
pub const AUD_PWRST: u32 = 0x6_5054;
/// ELD control and status for pipe A (the "DIP/ELD control" register).
pub const AUD_DIP_ELD_CTRL_ST_A: u32 = 0x6_50B4;
/// ELD data port for pipe A (write the ELD here, one dword at a time).
pub const AUD_HDMIW_HDMIEDID_A: u32 = 0x6_5050;
/// Per-port ELD-valid and output-enable bits.
pub const AUD_PIN_ELD_CP_VLD: u32 = 0x6_50C0;
/// Per-pipe stride of the audio registers above.
pub const AUD_PIPE_STRIDE: u32 = 0x100;

/// `AUD_CONFIG` fields: pixel-clock index (bits 19:16), explicit N enable,
/// and the 20-bit N split as upper (bits 27:20) and lower (bits 15:4).
pub mod aud_config {
    pub const CLOCK_SHIFT: u32 = 16;
    pub const CLOCK_MASK: u32 = 0xF << CLOCK_SHIFT;
    pub const N_VALUE_INDEX: u32 = 1 << 29;
    pub const N_PROG_ENABLE: u32 = 1 << 28;
    pub const UPPER_N_SHIFT: u32 = 20;
    pub const UPPER_N_MASK: u32 = 0xFF << UPPER_N_SHIFT;
    pub const LOWER_N_SHIFT: u32 = 4;
    pub const LOWER_N_MASK: u32 = 0xFFF << LOWER_N_SHIFT;
    pub const DISABLE_NCTS: u32 = 1 << 3;

    /// The `AUD_CONFIG` bits that carry an explicit `N` (and enable it).
    pub fn n_fields(n: u32) -> u32 {
        ((n >> 12) & 0xFF) << UPPER_N_SHIFT | (n & 0xFFF) << LOWER_N_SHIFT | N_PROG_ENABLE
    }

    /// The `N` an `AUD_CONFIG` value carries.
    pub fn n_value(config: u32) -> u32 {
        ((config & UPPER_N_MASK) >> UPPER_N_SHIFT) << 12 | (config & LOWER_N_MASK) >> LOWER_N_SHIFT
    }
}

/// Mode select of `TRANS_DDI_FUNC_CTL` (bits 26:24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DdiMode {
    Hdmi,
    Dvi,
    DpSst,
    DpMst,
    Fdi,
    Reserved(u8),
}

/// A decoded `TRANS_DDI_FUNC_CTL` (`// TO CONFIRM against dump`: enable bit
/// 31, DDI select bits 30:28, mode bits 26:24, bpc bits 22:20, sync polarity
/// bits 17:16). Audio needs [`DdiMode::Hdmi`]: a DVI-mode link is the case
/// that makes G2 rewrite a live pipe (plan section 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DdiFunc {
    pub enabled: bool,
    /// The DDI port feeding this transcoder: 0 = none, 1 = DDI B ... 4 = DDI E
    /// (`// TO CONFIRM against dump`: DDI A is reached through the eDP
    /// transcoder, not through this field).
    pub ddi_select: u8,
    pub mode: DdiMode,
    pub bits_per_component: Option<u8>,
    pub vsync_positive: bool,
    pub hsync_positive: bool,
}

impl DdiFunc {
    pub fn decode(v: u32) -> DdiFunc {
        DdiFunc {
            enabled: v & (1 << 31) != 0,
            ddi_select: ((v >> 28) & 7) as u8,
            mode: match ((v >> 24) & 7) as u8 {
                0 => DdiMode::Hdmi,
                1 => DdiMode::Dvi,
                2 => DdiMode::DpSst,
                3 => DdiMode::DpMst,
                4 => DdiMode::Fdi,
                r => DdiMode::Reserved(r),
            },
            bits_per_component: match (v >> 20) & 7 {
                0 => Some(8),
                1 => Some(10),
                2 => Some(6),
                3 => Some(12),
                _ => None,
            },
            vsync_positive: v & (1 << 17) != 0,
            hsync_positive: v & (1 << 16) != 0,
        }
    }
}

/// Decode `PIPESRC` (`(width - 1) << 16 | (height - 1)`), TO CONFIRM.
pub fn pipesrc_size(v: u32) -> (u32, u32) {
    ((v >> 16 & 0x1FFF) + 1, (v & 0x1FFF) + 1)
}

/// Decode a `HTOTAL`/`VTOTAL`-style register: active-1 in the low half,
/// total-1 in the high half. TO CONFIRM.
pub fn timing_pair(v: u32) -> (u32, u32) {
    ((v & 0xFFFF) + 1, (v >> 16) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn pipe_and_transcoder_addresses_follow_the_strides() {
        assert_eq!(PIPE_A_BASE + PIPECONF, 0x70008);
        assert_eq!(TRANS_A_BASE + trans::DDI_FUNC_CTL, TRANS_DDI_FUNC_CTL_A);
        assert_eq!(PLANE_CTL_1_A + 0xC0, PLANE_WM_1_A_0);
        assert_eq!(pipesrc_size(2559 << 16 | 1439), (2560, 1440));
        assert_eq!(timing_pair(2719 << 16 | 2559), (2560, 2720));
    }
}
