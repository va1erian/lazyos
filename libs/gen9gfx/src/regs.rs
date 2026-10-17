//! Gen9 display register offsets (BAR 0, bytes) for the plan's first stages:
//! G1 (adopt and observe the firmware's mode) and G2 (HDMI audio).
//!
//! Every constant is from memory of i915's `i915_reg.h` and Intel's Gen9
//! display PRM volumes. The groups marked `CONFIRMED` match the box's own
//! `intel_reg dump` (`docs/compat/kabylake/gpu/linux/01-intel_reg-dump.txt`,
//! read while i915 drove the panel, so each *value* is Linux's state, not the
//! firmware's); the tests decode that real state. The group marked
//! `TO CONFIRM` is not in that dump (`intel_reg dump` does not list the audio
//! block or the DDI buffer registers, and the raw range capture
//! `02-mmio-ranges.txt` failed; see `docs/kabylake-gpu-plan.md` section 10).
//! Nothing here is written by any code yet; the offsets that are laid out per
//! pipe or per port use the strides i915 uses.

// CONFIRMED against dump ------------------------------------------------------

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

/// Core display clock and the two LCPLLs.
pub const CDCLK_CTL: u32 = 0x4_6000;
pub const LCPLL1_CTL: u32 = 0x4_6010;
pub const LCPLL2_CTL: u32 = 0x4_6014;
/// Which DDI feeds transcoder A (`TRANS_CLK_SEL`; B at +4, C at +8). SKL+
/// has no `PORT_CLK_SEL`: the DDI's clock comes from `DPLL_CTRL2`.
pub const TRANS_CLK_SEL_A: u32 = 0x4_6140;

/// SKL-style shared DPLL control (DPLL1 to 3 share these) and status; the
/// per-PLL `CFGCR1`/`CFGCR2` of PLL `n` (1 to 3) sit 8 bytes apart.
pub const DPLL_CTRL1: u32 = 0x6_C058;
pub const DPLL_CTRL2: u32 = 0x6_C05C;
pub const DPLL_STATUS: u32 = 0x6_C060;
pub const DPLL1_CFGCR1: u32 = 0x6_C040;
pub const DPLL1_CFGCR2: u32 = 0x6_C044;
pub const DPLL_CFGCR_STRIDE: u32 = 8;

/// Transcoder function control of transcoder A, line time (for watermarks)
/// and the video data island packet (infoframe) enables.
pub const TRANS_DDI_FUNC_CTL_A: u32 = 0x6_0400;
pub const WM_LINETIME_A: u32 = 0x4_5270;
pub const VIDEO_DIP_CTL_A: u32 = 0x6_0200;
/// `VIDEO_DIP_CTL` enables, one per packet type (graphics control, AVI, SPD).
pub mod dip {
    pub const ENABLE_GCP: u32 = 1 << 16;
    pub const ENABLE_AVI: u32 = 1 << 12;
    pub const ENABLE_SPD: u32 = 1 << 0;
}

/// Power wells and DC states.
pub const PWR_WELL_CTL_BIOS: u32 = 0x4_5400;
pub const PWR_WELL_CTL_DRIVER: u32 = 0x4_5404;
pub const DC_STATE_EN: u32 = 0x4_5504;

/// Whether `DPLL_CTRL1` puts PLL `pll` (0 to 3) in HDMI mode: each PLL has
/// six bits at `pll * 6`: override (bit 0), DP link rate (3:1), HDMI mode (5).
pub fn dpll_ctrl1_hdmi_mode(ctrl1: u32, pll: u32) -> bool {
    pll < 4 && ctrl1 >> (pll * 6) & 0x20 != 0
}

/// Which PLL `DPLL_CTRL2` routes to DDI `port` (A = 0 ... E = 4), when that
/// port's override bit is set (otherwise the hardware's own default).
pub fn dpll_ctrl2_ddi_pll(ctrl2: u32, port: u32) -> Option<u32> {
    (port < 5 && ctrl2 >> (port * 3) & 1 != 0).then(|| ctrl2 >> (port * 3 + 1) & 3)
}

/// Core display clock in kHz from `CDCLK_CTL`: the decimal field (bits 10:0)
/// is `(cdclk - 1 MHz) / 500 kHz`. The box runs 337.5 MHz. The frequency
/// select bits (27:26) are not decoded here.
pub fn cdclk_khz(ctl: u32) -> u32 {
    (ctl & 0x7FF) * 500 + 1000
}

/// The DDI port (A = 0 ... E = 4) a `TRANS_CLK_SEL` value names (bits 31:29
/// hold `port + 1`), `None` for "no clock".
pub fn trans_clk_sel_port(v: u32) -> Option<u8> {
    match v >> 29 & 7 {
        0 => None,
        n => Some((n - 1) as u8),
    }
}

/// Pipe/transcoder `n` (0 = A, 1 = B, 2 = C) register block bases.
pub const fn pipe_base(pipe: u32) -> u32 {
    PIPE_A_BASE + pipe * PIPE_STRIDE
}
pub const fn trans_base(pipe: u32) -> u32 {
    TRANS_A_BASE + pipe * TRANS_STRIDE
}

/// `TRANS_CLK_SEL` of transcoder `pipe` (A 0x46140, B 0x46144, C 0x46148).
pub const fn trans_clk_sel(pipe: u32) -> u32 {
    TRANS_CLK_SEL_A + pipe * 4
}

/// Offsets inside a pipe block (`pipe_base`): the first universal plane
/// (`PLANE_*_1`, +0x180 .. +0x1AC) and the other plane's control as a
/// "something else is on" check (CONFIRMED: the box's dump).
pub mod plane1 {
    pub const CTL: u32 = 0x180;
    pub const STRIDE: u32 = 0x188;
    pub const POS: u32 = 0x18C;
    pub const SIZE: u32 = 0x190;
    pub const SURF: u32 = 0x19C;
    /// `PLANE_SURFLIVE`: the surface the hardware is scanning out now.
    pub const SURFLIVE: u32 = 0x1AC;
    pub const BUF_CFG: u32 = 0x27C;
}

/// `PLANE_CTL` bits (CONFIRMED: the box's `0xc404b020` is enabled, XRGB 8888,
/// Y tiled; pixel format bits 27:24, tiling bits 12:10).
pub mod plane_ctl {
    pub const ENABLE: u32 = 1 << 31;
    const FORMAT_SHIFT: u32 = 24;
    const FORMAT_MASK: u32 = 0xF << FORMAT_SHIFT;
    const TILING_MASK: u32 = 7 << 10;

    /// The pixel format field. Only the packed 32-bit formats a firmware
    /// framebuffer can have are named; every other value is `Other`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Format {
        /// 32 bits per pixel, blue in the low byte or red (`ORDER_RGBX` bit).
        Xrgb8888,
        Other(u8),
    }

    /// The tiling field: what `PLANE_STRIDE` is counted in.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Tiling {
        Linear,
        X,
        Y,
        Yf,
        Other(u8),
    }

    impl Tiling {
        /// Bytes per `PLANE_STRIDE` unit: 64 linear, 512 X tile, 128 Y tile.
        pub fn stride_unit(self) -> u32 {
            match self {
                Tiling::Linear => 64,
                Tiling::X => 512,
                Tiling::Y | Tiling::Yf => 128,
                Tiling::Other(_) => 64,
            }
        }
    }

    pub fn format(ctl: u32) -> Format {
        match ((ctl & FORMAT_MASK) >> FORMAT_SHIFT) as u8 {
            4 => Format::Xrgb8888,
            other => Format::Other(other),
        }
    }

    pub fn tiling(ctl: u32) -> Tiling {
        match ((ctl & TILING_MASK) >> 10) as u8 {
            0 => Tiling::Linear,
            1 => Tiling::X,
            4 => Tiling::Y,
            5 => Tiling::Yf,
            other => Tiling::Other(other),
        }
    }
}

/// SKL power well state bits in `PWR_WELL_CTL_*` (CONFIRMED against the dump:
/// BIOS `0x50000011`, driver `0xf0000033`): well `n`'s request is bit
/// `2 n + 1` and its state bit `2 n`, with PW1 at index 14 and PW2 at 15,
/// the MISC IO well at 0.
pub mod pwr_well {
    pub const PW1_STATE: u32 = 1 << 28;
    pub const PW2_STATE: u32 = 1 << 30;
    pub const MISC_IO_STATE: u32 = 1 << 0;
}

/// The pixel clock in kHz a HDMI-mode SKL DPLL (1 to 3) generates, from its
/// `CFGCR1` and `CFGCR2`, as i915's `skl_ddi_pll_get_freq` computes it:
/// `(DCO integer + DCO fraction / 2^15) * 24 MHz / (p0 * p1 * p2 * 5)`.
/// CONFIRMED against the box's `DPLL1_CFGCR1 0x8046675b`, `DPLL1_CFGCR2
/// 0x147`: 8.3346 GHz DCO / 30 = 277820 kHz, the 2560x1440 at 70 Hz clock.
/// `None` when a divider field is reserved or the DCO is not enabled.
pub fn dpll_hdmi_clock_khz(cfgcr1: u32, cfgcr2: u32) -> Option<u32> {
    const REF_KHZ: u64 = 24_000;
    if cfgcr1 & (1 << 31) == 0 {
        return None;
    }
    let integer = u64::from(cfgcr1 & 0x1FF);
    let fraction = u64::from((cfgcr1 >> 9) & 0xFFFF);
    let p0 = match (cfgcr2 >> 2) & 7 {
        0 => 1,
        1 => 2,
        2 => 3,
        4 => 7,
        _ => return None,
    };
    let p2 = match (cfgcr2 >> 5) & 3 {
        0 => 5,
        1 => 2,
        2 => 3,
        _ => 1,
    };
    let p1 = if cfgcr2 & (1 << 7) != 0 {
        u64::from((cfgcr2 >> 8) & 0xFF)
    } else {
        1
    };
    let dco = integer * REF_KHZ + ((fraction * REF_KHZ) >> 15);
    let divider = p0 * p1 * p2 * 5;
    // Rounded: the fraction field is a 15-bit approximation (the box's
    // DCO is 8334599.8 kHz for an 8334600 kHz target).
    (divider != 0 && p1 != 0).then(|| ((dco + divider / 2) / divider) as u32)
}

// TO CONFIRM against dump -----------------------------------------------------

/// `PIPESRC` (the pipe's source size) inside the transcoder block, `0x6001C`
/// for pipe A. `intel_reg dump` does not list it.
pub const TRANS_PIPESRC: u32 = 0x1C;

/// Per-DDI clock select of the pre-Skylake path (A at 0x46100, 4 bytes
/// apart). Gen9 routes the DDI clock through `DPLL_CTRL2` instead, so this is
/// only here so a dump that shows it can be read; the Gen9 dump does not.
pub const PORT_CLK_SEL_A: u32 = 0x4_6100;

/// DDI buffer control for DDI A, stride 0x100 per port.
pub const DDI_BUF_CTL_A: u32 = 0x6_4000;
pub const DDI_BUF_STRIDE: u32 = 0x100;
pub const DDI_BUF_CTL_ENABLE: u32 = 1 << 31;

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

/// A decoded `TRANS_DDI_FUNC_CTL` (CONFIRMED: the box's dump reads
/// `0x90030000` for "enabled, DDIB, HDMI, 8 bpc, +VSync, +HSync"; enable bit
/// 31, DDI select bits 30:28, mode bits 26:24, bpc bits 22:20, sync polarity
/// bits 17:16). Audio needs [`DdiMode::Hdmi`]: a DVI-mode link is the case
/// that makes G2 rewrite a live pipe (plan section 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DdiFunc {
    pub enabled: bool,
    /// The DDI port feeding this transcoder: 0 = none, 1 = DDI B ... 4 = DDI E
    /// (DDI A is reached through the eDP transcoder, not through this field).
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

/// Decode `PIPESRC` (`(width - 1) << 16 | (height - 1)`). Not in the dump
/// (`intel_reg` does not list it), so TO CONFIRM; `PLANE_SIZE` has the halves
/// the other way round (`(height - 1) << 16`).
pub fn pipesrc_size(v: u32) -> (u32, u32) {
    ((v >> 16 & 0x1FFF) + 1, (v & 0x1FFF) + 1)
}

/// Decode `PLANE_SIZE_n_x` (`(height - 1) << 16 | (width - 1)`, CONFIRMED:
/// `PLANE_SIZE_1_A` reads `0x059f09ff`, 2560x1440), as `(width, height)`.
pub fn plane_size(v: u32) -> (u32, u32) {
    ((v & 0xFFFF) + 1, (v >> 16) + 1)
}

/// Decode a `HTOTAL`/`VTOTAL`-style register: active-1 in the low half,
/// total-1 in the high half (CONFIRMED: `TRANS_HTOTAL_A` reads `0x0a6d09ff`,
/// 2560 active, 2670 total).
pub fn timing_pair(v: u32) -> (u32, u32) {
    ((v & 0xFFFF) + 1, (v >> 16) + 1)
}
