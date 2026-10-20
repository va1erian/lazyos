//! What the firmware left on the display: read the state registers through
//! an [`Mmio`] and decode them (`docs/kabylake-gpu-plan.md`, stage G1,
//! "Adopt + observe").
//!
//! **Reads only.** [`Display::read`] and [`read_fb_map`] call
//! [`Mmio::read32`] and nothing else, on state registers (no CRC, FIFO or
//! index register that advances on a read; the dwords are all inside the
//! ranges `libs/pciprobe` lets `dbgd` read for the device, a test per
//! generation holds the two together), so adopting the mode cannot change
//! the mode. Nothing here decides what to do with it; the machine side
//! (`gpud`) traces it and serves it.
//!
//! Two display generations are decoded into the same [`Adopted`]: Gen9
//! ([`crate::gen9`], the Kaby Lake HD 620) and display version 13
//! ([`crate::d13`], the Alder Lake-P UHD Graphics). Their pipe, transcoder
//! timing and plane blocks share offsets and layout (read here, by
//! [`RegFile::read_pipe`]); clocks, power wells and the DDI routing differ.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::mmio::Mmio;
use crate::regs::{self, plane1, plane_ctl, DdiFunc, DdiMode};

/// Where the global GTT starts in BAR 0: the lower 8 MiB are registers.
pub const GGTT_OFFSET: u32 = 0x80_0000;
/// Page-table entries the 8 MiB GGTT holds (8 bytes each).
pub const GGTT_ENTRIES: u32 = 1 << 20;
/// The most framebuffer pages [`read_fb_map`] walks (64 MiB).
pub const MAX_FB_PAGES: u32 = 16 * 1024;

/// Pipe letters, A to D (Gen9 has three pipes, display 13 four).
pub(crate) const PIPE_NAMES: [&str; 4] = ["A", "B", "C", "D"];

/// The display engine of a device `gpud` adopts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    /// Gen9 (Skylake to Coffee Lake): the HD Graphics 620, `8086:5916`.
    Gen9,
    /// Display version 13 (Alder Lake-P): UHD Graphics, `8086:46b3`.
    D13,
}

impl Display {
    /// The display engine of the Intel device `device`, when it is one of
    /// the parts whose registers were checked (`libs/pciprobe` lists their
    /// ranges): `None` for anything else.
    pub fn for_device(device: u16) -> Option<Display> {
        match device {
            0x5916 => Some(Display::Gen9),
            0x46B3 => Some(Display::D13),
            _ => None,
        }
    }

    /// The configuration-space offset of `BDSM` (the stolen memory base):
    /// a dword at 0x5C on Gen9, the low dword of a qword at 0xC0 from
    /// display 12 on.
    pub fn bdsm_config_offset(self) -> u64 {
        match self {
            Display::Gen9 => 0x5C,
            Display::D13 => 0xC0,
        }
    }

    /// Read the display state the firmware left (reads only).
    pub fn read(self, mmio: &mut impl Mmio) -> RegFile {
        match self {
            Display::Gen9 => crate::gen9::read(mmio),
            Display::D13 => crate::d13::read(mmio),
        }
    }

    /// Decode a register file [`Display::read`] produced.
    pub fn decode(self, file: &RegFile) -> Adopted {
        match self {
            Display::Gen9 => crate::gen9::decode(file),
            Display::D13 => crate::d13::decode(file),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Display::Gen9 => "gen9",
            Display::D13 => "d13",
        }
    }
}

/// One register read: its name (`TRANS_HTOTAL_A`), byte offset, value and
/// whether the offset is confirmed by a dump of the box (`false`: an offset
/// from memory of i915 that no capture has shown yet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegValue {
    pub name: String,
    pub offset: u32,
    pub value: u32,
    pub confirmed: bool,
}

/// Every register [`Display::read`] reads, in read order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegFile {
    pub regs: Vec<RegValue>,
    /// Blocks not read because their power well was down (or the bus was
    /// dead): `PIPE_B`, `AUDIO`, ...
    pub gated: Vec<String>,
}

impl RegFile {
    /// Read one pipe's transcoder timings and function, pipe configuration
    /// and first plane: the same offsets on both generations.
    pub(crate) fn read_pipe(&mut self, mmio: &mut impl Mmio, pipe: u32) {
        let id = PIPE_NAMES[pipe as usize];
        let (p, t) = (regs::pipe_base(pipe), regs::trans_base(pipe));
        let n = |what: &str| format!("{what}_{id}");
        self.add(mmio, &n("TRANS_CLK_SEL"), regs::trans_clk_sel(pipe), true);
        self.add(mmio, &n("PIPECONF"), p + regs::PIPECONF, true);
        self.add(mmio, &n("TRANS_HTOTAL"), t + regs::trans::HTOTAL, true);
        self.add(mmio, &n("TRANS_HBLANK"), t + regs::trans::HBLANK, true);
        self.add(mmio, &n("TRANS_HSYNC"), t + regs::trans::HSYNC, true);
        self.add(mmio, &n("TRANS_VTOTAL"), t + regs::trans::VTOTAL, true);
        self.add(mmio, &n("TRANS_VBLANK"), t + regs::trans::VBLANK, true);
        self.add(mmio, &n("TRANS_VSYNC"), t + regs::trans::VSYNC, true);
        self.add(mmio, &n("PIPESRC"), t + regs::TRANS_PIPESRC, false);
        self.add(
            mmio,
            &n("TRANS_DDI_FUNC_CTL"),
            t + regs::trans::DDI_FUNC_CTL,
            true,
        );
        self.add(mmio, &n("VIDEO_DIP_CTL"), t + 0x200, true);
        self.add(mmio, &n("PLANE_CTL_1"), p + plane1::CTL, true);
        self.add(mmio, &n("PLANE_STRIDE_1"), p + plane1::STRIDE, true);
        self.add(mmio, &n("PLANE_POS_1"), p + plane1::POS, true);
        self.add(mmio, &n("PLANE_SIZE_1"), p + plane1::SIZE, true);
        self.add(mmio, &n("PLANE_SURF_1"), p + plane1::SURF, true);
        self.add(mmio, &n("PLANE_SURFLIVE_1"), p + plane1::SURFLIVE, true);
        self.add(mmio, &n("PLANE_BUF_CFG_1"), p + plane1::BUF_CFG, true);
    }

    pub(crate) fn add(&mut self, mmio: &mut impl Mmio, name: &str, offset: u32, confirmed: bool) {
        let value = mmio.read32(offset);
        self.regs.push(RegValue {
            name: String::from(name),
            offset,
            value,
            confirmed,
        });
    }

    /// The value read at `offset` (0 when it was not read).
    pub fn get(&self, offset: u32) -> u32 {
        self.regs
            .iter()
            .find(|reg| reg.offset == offset)
            .map_or(0, |reg| reg.value)
    }

    /// Whether the device answered like hardware: a bus that returns all
    /// ones for the always-on power well registers and `CDCLK_CTL` (the same
    /// offsets on both generations) is not there (a function with memory
    /// decode off, or a dead link).
    pub fn looks_dead(&self) -> bool {
        [
            regs::PWR_WELL_CTL_BIOS,
            regs::PWR_WELL_CTL_DRIVER,
            regs::CDCLK_CTL,
        ]
        .iter()
        .all(|&offset| self.get(offset) == u32::MAX)
    }
}

/// One timing axis: values are counts in pixels (horizontal) or lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Axis {
    pub active: u32,
    pub total: u32,
    pub blank_start: u32,
    pub blank_end: u32,
    pub sync_start: u32,
    pub sync_end: u32,
}

impl Axis {
    fn decode(total: u32, blank: u32, sync: u32) -> Axis {
        let (active, total) = regs::timing_pair(total);
        let (blank_start, blank_end) = regs::timing_pair(blank);
        let (sync_start, sync_end) = regs::timing_pair(sync);
        Axis {
            active,
            total,
            blank_start,
            blank_end,
            sync_start,
            sync_end,
        }
    }
}

/// The first universal plane of a pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    pub enabled: bool,
    pub format: plane_ctl::Format,
    pub tiling: plane_ctl::Tiling,
    /// Row pitch in bytes (`PLANE_STRIDE` in the tiling's unit).
    pub stride_bytes: u32,
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    /// GGTT offset of the surface being programmed (`PLANE_SURF`).
    pub surf: u32,
    /// The surface being scanned out (`PLANE_SURFLIVE`).
    pub surf_live: u32,
}

impl Plane {
    fn decode(file: &RegFile, pipe: u32) -> Plane {
        let base = regs::pipe_base(pipe);
        let ctl = file.get(base + plane1::CTL);
        let tiling = plane_ctl::tiling(ctl);
        let size = file.get(base + plane1::SIZE);
        let pos = file.get(base + plane1::POS);
        Plane {
            enabled: ctl & plane_ctl::ENABLE != 0,
            format: plane_ctl::format(ctl),
            tiling,
            stride_bytes: (file.get(base + plane1::STRIDE) & 0x7FF) * tiling.stride_unit(),
            // `PLANE_SIZE` is `(height - 1) << 16 | (width - 1)`.
            width: (size & 0x1FFF) + 1,
            height: ((size >> 16) & 0x1FFF) + 1,
            x: pos & 0xFFFF,
            y: pos >> 16,
            surf: file.get(base + plane1::SURF) & !0xFFF,
            surf_live: file.get(base + plane1::SURFLIVE) & !0xFFF,
        }
    }

    /// Bytes of memory the surface occupies: the pitch times the height
    /// rounded to whole tile rows (8 lines for X, 32 for Y).
    pub fn bytes(&self) -> u64 {
        let tile_rows = match self.tiling {
            plane_ctl::Tiling::X => 8,
            plane_ctl::Tiling::Y | plane_ctl::Tiling::Yf => 32,
            _ => 1,
        };
        u64::from(self.stride_bytes) * u64::from(self.height.div_ceil(tile_rows) * tile_rows)
    }
}

/// What a generation's decoder found about the clock of a running pipe's
/// port.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortClock {
    /// The DPLL the port is routed to (Gen9: `DPLL_CTRL2`, 0 = the CDCLK
    /// PLL; display 13: `DPCLKA_CFGCR0`, 0 = DPLL0, 1 = DPLL1).
    pub pll: Option<u8>,
    /// The PLL is set up to clock this port: Gen9's `DPLL_CTRL1` puts it in
    /// HDMI mode; display 13's `DPLL_ENABLE` reads enabled and locked.
    pub pll_ready: bool,
    /// The port's clock is gated off (Gen9 `DPLL_CTRL2`, display 13
    /// `DPCLKA_CFGCR0`).
    pub ddi_clock_off: bool,
    /// The pixel clock in kHz from the PLL's dividers.
    pub pixel_clock_khz: Option<u32>,
}

/// A pipe that is running: enabled, with its transcoder function on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivePipe {
    /// 0 = A, 1 = B, ... (the transcoder has the same index).
    pub pipe: u8,
    pub func: DdiFunc,
    /// The DDI port, 0 = A, 1 = B, ... (on display 13, 3 to 6 are the
    /// Type-C ports TC1 to TC4).
    pub port: Option<u8>,
    pub pll: Option<u8>,
    pub pll_ready: bool,
    pub ddi_clock_off: bool,
    pub pixel_clock_khz: Option<u32>,
    pub h: Axis,
    pub v: Axis,
    /// The pipe source size (`PIPESRC`), `None` when the register disagrees
    /// with the transcoder's active size (its offset is still to confirm on
    /// Gen9).
    pub source: Option<(u32, u32)>,
    pub plane: Plane,
    /// `VIDEO_DIP_CTL` of the transcoder (GCP, AVI, SPD enables).
    pub dip: u32,
}

impl ActivePipe {
    /// Decode the parts of `pipe` both generations share, given what the
    /// generation's decoder found about the function and the port.
    pub(crate) fn decode(
        file: &RegFile,
        pipe: u32,
        func: DdiFunc,
        port: Option<u8>,
        clock: PortClock,
    ) -> ActivePipe {
        let t = regs::trans_base(pipe);
        let h = Axis::decode(
            file.get(t + regs::trans::HTOTAL),
            file.get(t + regs::trans::HBLANK),
            file.get(t + regs::trans::HSYNC),
        );
        let v = Axis::decode(
            file.get(t + regs::trans::VTOTAL),
            file.get(t + regs::trans::VBLANK),
            file.get(t + regs::trans::VSYNC),
        );
        // Take `PIPESRC` only if it agrees with the transcoder's active size
        // (a wrong offset reads another register).
        let source = {
            let source = regs::pipesrc_size(file.get(t + regs::TRANS_PIPESRC));
            (source == (h.active, v.active)).then_some(source)
        };
        ActivePipe {
            pipe: pipe as u8,
            func,
            port,
            pll: clock.pll,
            pll_ready: clock.pll_ready,
            ddi_clock_off: clock.ddi_clock_off,
            pixel_clock_khz: clock.pixel_clock_khz,
            h,
            v,
            source,
            plane: Plane::decode(file, pipe),
            dip: file.get(t + 0x200),
        }
    }

    /// The refresh rate in millihertz (`clock / (htotal * vtotal)`).
    pub fn refresh_millihz(&self) -> Option<u32> {
        let clock = u64::from(self.pixel_clock_khz?);
        let frame = u64::from(self.h.total) * u64::from(self.v.total);
        (frame != 0).then(|| (clock * 1_000_000 / frame) as u32)
    }

    /// The link is HDMI mode: infoframes and audio need no mode set.
    pub fn hdmi_link(&self) -> bool {
        self.func.enabled && self.func.mode == DdiMode::Hdmi
    }

    pub fn dip_gcp(&self) -> bool {
        self.dip & regs::dip::ENABLE_GCP != 0
    }

    pub fn dip_avi(&self) -> bool {
        self.dip & regs::dip::ENABLE_AVI != 0
    }

    pub fn dip_spd(&self) -> bool {
        self.dip & regs::dip::ENABLE_SPD != 0
    }
}

/// The display state decoded from a [`RegFile`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adopted {
    pub display: Display,
    /// The first running pipe, if any.
    pub active: Option<ActivePipe>,
    /// Bit `n` set: pipe `n` is enabled (`PIPECONF` enable), whether or not
    /// it is the one decoded.
    pub pipes_enabled: u8,
    /// The eDP transcoder's function is enabled (Gen9's DDI A; display 13
    /// has no eDP transcoder).
    pub edp_enabled: bool,
    pub cdclk_khz: u32,
    /// The CDCLK PLL (Gen9 LCPLL1, display 13 `CDCLK_PLL_ENABLE`) enabled
    /// and locked.
    pub cdclk_pll_locked: bool,
    pub power_well_1: bool,
    pub power_well_2: bool,
    pub dc_state_en: u32,
}

/// The pipes of `file` whose `PIPECONF` enable is set, as a mask, and the
/// first of them `decode_pipe` accepts.
pub(crate) fn running_pipes(
    file: &RegFile,
    pipes: u32,
    mut decode_pipe: impl FnMut(u32) -> Option<ActivePipe>,
) -> (u8, Option<ActivePipe>) {
    let mut enabled = 0u8;
    let mut active = None;
    for pipe in 0..pipes {
        let conf = file.get(regs::pipe_base(pipe) + regs::PIPECONF);
        if conf & regs::PIPECONF_ENABLE == 0 {
            continue;
        }
        enabled |= 1 << pipe;
        if active.is_none() {
            active = decode_pipe(pipe);
        }
    }
    (enabled, active)
}

pub use crate::fbmap::{read_fb_map, FbMap};
