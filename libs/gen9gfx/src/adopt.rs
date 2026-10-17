//! What the firmware left on the display: read the state registers through
//! an [`Mmio`] and decode them (`docs/kabylake-gpu-plan.md`, stage G1,
//! "Adopt + observe").
//!
//! **Reads only.** [`RegFile::read`] and [`read_fb_map`] call
//! [`Mmio::read32`] and nothing else, on state registers (no CRC, FIFO or
//! index register that advances on a read; the dwords are all inside the
//! ranges `libs/pciprobe` lets `dbgd` read, a test holds the two together),
//! so adopting the mode cannot change the mode. Nothing here decides what to
//! do with it; the machine side (`gpud`) traces it and serves it.
//!
//! The decoders are pure functions of the dword values, tested against the
//! box's own `intel_reg dump` (`docs/compat/kabylake/gpu/linux/`): that
//! capture is Linux's state, and what the *firmware* leaves is what a LazyOS
//! boot of the box will show, so every value in the tests is a known
//! register content, never an assertion about the firmware.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::mmio::Mmio;
use crate::regs::{self, plane1, plane_ctl, DdiFunc, DdiMode};

/// Pipes (and transcoders A to C) a Gen9 GT2 has.
pub const PIPES: usize = 3;
/// DDI ports A to E.
pub const DDI_PORTS: usize = 5;
/// Where the global GTT starts in BAR 0: the lower 8 MiB are registers.
pub const GGTT_OFFSET: u32 = 0x80_0000;
/// Page-table entries the 8 MiB GGTT holds (8 bytes each).
pub const GGTT_ENTRIES: u32 = 1 << 20;
/// The most framebuffer pages [`read_fb_map`] walks (64 MiB).
pub const MAX_FB_PAGES: u32 = 16 * 1024;

// Offsets in the group the box's dump did not show, kept here (not in
// `regs`) because they are only ever read, and only to print: a wrong one
// shows another register of the same block. Forcewake request registers are
// believed to be i915's `FORCEWAKE_BLITTER_GEN9` (0xA188), `_MEDIA_GEN9`
// (0xA270) and `_RENDER_GEN9` (0xA278) and the acknowledges
// `FORCEWAKE_ACK_RENDER_GEN9` (0xD84) and `_MEDIA_GEN9` (0xD88): which of
// the two is which is not settled, so they carry their address as a name.
const FW_REQUESTS: [u32; 3] = [0xA188, 0xA270, 0xA278];
const FW_ACKS: [u32; 2] = [0xD84, 0xD88];
const EDP_DDI_FUNC_CTL: u32 = 0x6_F400;
const EDP_TRANS_CONF: u32 = 0x7_F008;

/// One register read: its name (`TRANS_HTOTAL_A`), byte offset, value and
/// whether the offset is confirmed by the box's dump (`false`: an offset
/// from memory of i915 that the survey has not shown yet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegValue {
    pub name: String,
    pub offset: u32,
    pub value: u32,
    pub confirmed: bool,
}

/// Every register [`RegFile::read`] reads, in read order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegFile {
    pub regs: Vec<RegValue>,
    /// Blocks not read because their power well was down (or the bus was
    /// dead): `PIPE_B`, `AUDIO`, ...
    pub gated: Vec<String>,
}

const PIPE_NAMES: [&str; PIPES] = ["A", "B", "C"];
const DDI_NAMES: [&str; DDI_PORTS] = ["A", "B", "C", "D", "E"];

impl RegFile {
    /// Read the display state of the whole device: power wells, CDCLK, the
    /// DPLLs, the three pipes with their transcoder, DDI function and first
    /// plane, the DDI buffers and the audio block (the last two with
    /// offsets still to confirm), and the forcewake registers.
    pub fn read(mmio: &mut impl Mmio) -> RegFile {
        let mut file = RegFile::default();
        file.add(mmio, "PWR_WELL_BIOS", regs::PWR_WELL_CTL_BIOS, true);
        file.add(mmio, "PWR_WELL_DRIVER", regs::PWR_WELL_CTL_DRIVER, true);
        file.add(mmio, "DC_STATE_EN", regs::DC_STATE_EN, true);
        file.add(mmio, "CDCLK_CTL", regs::CDCLK_CTL, true);
        file.add(mmio, "LCPLL1_CTL", regs::LCPLL1_CTL, true);
        file.add(mmio, "LCPLL2_CTL", regs::LCPLL2_CTL, true);
        file.add(mmio, "DPLL_CTRL1", regs::DPLL_CTRL1, true);
        file.add(mmio, "DPLL_CTRL2", regs::DPLL_CTRL2, true);
        file.add(mmio, "DPLL_STATUS", regs::DPLL_STATUS, true);
        for pll in 0..3 {
            let base = regs::DPLL1_CFGCR1 + pll * regs::DPLL_CFGCR_STRIDE;
            file.add(mmio, &format!("DPLL{}_CFGCR1", pll + 1), base, true);
            file.add(mmio, &format!("DPLL{}_CFGCR2", pll + 1), base + 4, true);
        }
        // A register behind a power well that is down may not answer
        // cleanly, so the blocks below are read only while their well is
        // up: pipe A, the eDP transcoder and DDI A are in well 1, pipes B
        // and C, DDI B to E and the audio block in well 2. A bus that
        // answers all ones everywhere is not read further at all.
        let well = file.get(regs::PWR_WELL_CTL_DRIVER);
        let dead = file.looks_dead();
        let pw1 = !dead && well & regs::pwr_well::PW1_STATE != 0;
        let pw2 = !dead && well & regs::pwr_well::PW2_STATE != 0;
        for pipe in 0..PIPES as u32 {
            if if pipe == 0 { pw1 } else { pw2 } {
                file.read_pipe(mmio, pipe);
            } else {
                file.gated
                    .push(format!("PIPE_{}", PIPE_NAMES[pipe as usize]));
            }
        }
        if pw1 {
            file.add(mmio, "TRANS_DDI_FUNC_CTL_EDP", EDP_DDI_FUNC_CTL, true);
            file.add(mmio, "TRANS_CONF_EDP", EDP_TRANS_CONF, true);
        } else {
            file.gated.push(String::from("TRANS_EDP"));
        }
        for port in 0..DDI_PORTS as u32 {
            // DDI A and E are in well 1 (E on parts that have it).
            let up = if port == 0 || port == 4 { pw1 } else { pw2 };
            let name = format!("DDI_BUF_CTL_{}", DDI_NAMES[port as usize]);
            if up {
                let offset = regs::DDI_BUF_CTL_A + port * regs::DDI_BUF_STRIDE;
                file.add(mmio, &name, offset, false);
            } else {
                file.gated.push(name);
            }
        }
        if pw2 {
            file.add(mmio, "AUD_CONFIG_A", regs::AUD_CONFIG_A, false);
            file.add(mmio, "AUD_MISC_CTRL_A", regs::AUD_MISC_CTRL_A, false);
            file.add(mmio, "AUD_VID_DID", regs::AUD_VID_DID, false);
            file.add(mmio, "AUD_RATE_CAP", regs::AUD_RATE_CAP, false);
            file.add(
                mmio,
                "AUD_DIP_ELD_CTRL_ST_A",
                regs::AUD_DIP_ELD_CTRL_ST_A,
                false,
            );
            file.add(mmio, "AUD_PIN_ELD_CP_VLD", regs::AUD_PIN_ELD_CP_VLD, false);
        } else {
            file.gated.push(String::from("AUDIO"));
        }
        if dead {
            return file;
        }
        for offset in FW_REQUESTS {
            file.add(mmio, &format!("FW_REQ_{offset:04X}"), offset, false);
        }
        for offset in FW_ACKS {
            file.add(mmio, &format!("FW_ACK_{offset:04X}"), offset, false);
        }
        file
    }

    fn read_pipe(&mut self, mmio: &mut impl Mmio, pipe: u32) {
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

    fn add(&mut self, mmio: &mut impl Mmio, name: &str, offset: u32, confirmed: bool) {
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
    /// ones for the always-on power well registers is not there (a function
    /// with memory decode off, or a dead link).
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

/// A pipe that is running: enabled, with its transcoder function on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivePipe {
    /// 0 = A, 1 = B, 2 = C (the transcoder has the same index).
    pub pipe: u8,
    pub func: DdiFunc,
    /// The DDI port, 0 = A to 4 = E: the function's select field (DDI A is
    /// reached through the eDP transcoder, not through this one), or the
    /// clock select register when the function has none.
    pub port: Option<u8>,
    /// The DPLL `DPLL_CTRL2` routes the port to (0 = the CDCLK PLL).
    pub pll: Option<u8>,
    /// `DPLL_CTRL1` puts that PLL in HDMI mode.
    pub pll_hdmi_mode: bool,
    /// `DPLL_CTRL2` has the port's clock gated off.
    pub ddi_clock_off: bool,
    /// The pixel clock in kHz, from the PLL's dividers (HDMI mode only).
    pub pixel_clock_khz: Option<u32>,
    pub h: Axis,
    pub v: Axis,
    /// The pipe source size (`PIPESRC`), `None` when the register is not
    /// plausible (its offset is still to confirm).
    pub source: Option<(u32, u32)>,
    pub plane: Plane,
    /// `VIDEO_DIP_CTL` of the transcoder (GCP, AVI, SPD enables).
    pub dip: u32,
}

impl ActivePipe {
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
    /// The first running pipe, if any.
    pub active: Option<ActivePipe>,
    /// Bit `n` set: pipe `n` is enabled (`PIPECONF` enable), whether or not
    /// it is the one decoded.
    pub pipes_enabled: u8,
    /// The eDP transcoder's function is enabled (DDI A; not decoded).
    pub edp_enabled: bool,
    pub cdclk_khz: u32,
    /// LCPLL1 (the CDCLK PLL) enabled and locked.
    pub cdclk_pll_locked: bool,
    pub power_well_1: bool,
    pub power_well_2: bool,
    pub dc_state_en: u32,
}

impl Adopted {
    pub fn decode(file: &RegFile) -> Adopted {
        let mut pipes_enabled = 0u8;
        let mut active = None;
        for pipe in 0..PIPES as u32 {
            let conf = file.get(regs::pipe_base(pipe) + regs::PIPECONF);
            if conf & regs::PIPECONF_ENABLE == 0 {
                continue;
            }
            pipes_enabled |= 1 << pipe;
            if active.is_none() {
                active = decode_pipe(file, pipe);
            }
        }
        let lcpll1 = file.get(regs::LCPLL1_CTL);
        let well = file.get(regs::PWR_WELL_CTL_DRIVER);
        Adopted {
            active,
            pipes_enabled,
            edp_enabled: file.get(EDP_DDI_FUNC_CTL) & (1 << 31) != 0,
            cdclk_khz: regs::cdclk_khz(file.get(regs::CDCLK_CTL)),
            // PLL_ENABLE (31) and PLL_LOCK (30).
            cdclk_pll_locked: lcpll1 >> 30 == 3,
            power_well_1: well & regs::pwr_well::PW1_STATE != 0,
            power_well_2: well & regs::pwr_well::PW2_STATE != 0,
            dc_state_en: file.get(regs::DC_STATE_EN),
        }
    }
}

fn decode_pipe(file: &RegFile, pipe: u32) -> Option<ActivePipe> {
    let t = regs::trans_base(pipe);
    let func = DdiFunc::decode(file.get(t + regs::trans::DDI_FUNC_CTL));
    if !func.enabled {
        return None;
    }
    let by_clock_select = regs::trans_clk_sel_port(file.get(regs::trans_clk_sel(pipe)));
    let port = match func.ddi_select {
        0 => by_clock_select,
        select => Some(select),
    };
    let ctrl1 = file.get(regs::DPLL_CTRL1);
    let ctrl2 = file.get(regs::DPLL_CTRL2);
    let pll = port
        .and_then(|port| regs::dpll_ctrl2_ddi_pll(ctrl2, u32::from(port)))
        .map(|pll| pll as u8);
    let pll_hdmi_mode = pll.is_some_and(|pll| regs::dpll_ctrl1_hdmi_mode(ctrl1, u32::from(pll)));
    // `DDI_CLK_OFF` of port `n` is bit `3 n + 15`; ports are A to E only.
    let ddi_clock_off = port
        .filter(|&port| usize::from(port) < DDI_PORTS)
        .is_some_and(|port| ctrl2 & (1 << (u32::from(port) * 3 + 15)) != 0);
    // Only DPLL1 to 3 have dividers; DPLL0 is the CDCLK PLL.
    let pixel_clock_khz = pll.filter(|_| pll_hdmi_mode).and_then(|pll| match pll {
        1..=3 => {
            let base = regs::DPLL1_CFGCR1 + (u32::from(pll) - 1) * regs::DPLL_CFGCR_STRIDE;
            regs::dpll_hdmi_clock_khz(file.get(base), file.get(base + 4))
        }
        _ => None,
    });
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
    // `PIPESRC` is not in the box's dump; take it only if it agrees with
    // the transcoder's active size (a wrong offset reads another register).
    let source = {
        let source = regs::pipesrc_size(file.get(t + regs::TRANS_PIPESRC));
        (source == (h.active, v.active)).then_some(source)
    };
    Some(ActivePipe {
        pipe: pipe as u8,
        func,
        port,
        pll,
        pll_hdmi_mode,
        ddi_clock_off,
        pixel_clock_khz,
        h,
        v,
        source,
        plane: Plane::decode(file, pipe),
        dip: file.get(t + 0x200),
    })
}

/// Where a framebuffer's pages are, from its GGTT entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FbMap {
    /// Pages the surface spans, and how many entries were read.
    pub pages: u32,
    pub present: u32,
    /// Physical address of the first and last page (0 when not present).
    pub first: u64,
    pub last: u64,
    /// Every entry present and each page the next after the last.
    pub contiguous: bool,
}

/// Read the GGTT entries of the surface at GGTT offset `surf`, `bytes` long
/// (at most [`MAX_FB_PAGES`] pages are walked).
///
/// A Gen8+ entry is 64 bits: present in bit 0, the page's physical address
/// in bits 38:12 (cache control bits sit below the address and are not
/// decoded). Entries are read as two dwords; a surface outside the table
/// reads nothing.
pub fn read_fb_map(mmio: &mut impl Mmio, surf: u32, bytes: u64) -> FbMap {
    let first_entry = surf >> 12;
    let pages = bytes.div_ceil(4096).min(u64::from(MAX_FB_PAGES)) as u32;
    let mut map = FbMap {
        pages,
        contiguous: pages > 0,
        ..FbMap::default()
    };
    if pages == 0 || first_entry >= GGTT_ENTRIES || pages > GGTT_ENTRIES - first_entry {
        map.contiguous = false;
        return map;
    }
    let mut previous = 0u64;
    for index in 0..pages {
        let at = GGTT_OFFSET + (first_entry + index) * 8;
        let low = mmio.read32(at);
        let high = mmio.read32(at + 4);
        let entry = u64::from(high) << 32 | u64::from(low);
        if entry & 1 == 0 {
            map.contiguous = false;
            continue;
        }
        let phys = entry & 0x7F_FFFF_F000;
        if map.present == 0 {
            map.first = phys;
        } else if phys != previous + 4096 {
            map.contiguous = false;
        }
        map.present += 1;
        map.last = phys;
        previous = phys;
    }
    map.contiguous &= map.present == pages;
    map
}
