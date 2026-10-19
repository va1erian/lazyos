//! Display version 13's half of [`crate::adopt`] (Alder Lake-P, the UHD
//! Graphics `8086:46b3` of the CHUWI LarkBox X): which registers are read and
//! how its clocks, power wells and DDI routing decode.
//!
//! The pipe, transcoder timing and plane blocks have Gen9's offsets and
//! layout; what differs, as i915 names it for display 12 and 13:
//!
//! * power wells are numbered by index, request bit `2 i + 1` and state bit
//!   `2 i` in `PWR_WELL_CTL*`: PW1 is index 0, PW2 index 1 and the pipe wells
//!   PW_A to PW_D indices 5 to 8 (`XELPD_PW_CTL_IDX_PW_*`);
//! * CDCLK comes from its own PLL (`CDCLK_PLL_ENABLE`: ratio x the reference
//!   clock), divided by `CDCLK_CTL`'s CD2X field and by two; the reference
//!   is strapped in `DSSM`;
//! * `TRANS_DDI_FUNC_CTL` names the port as `port + 1` in bits 30:27 and
//!   `TRANS_CLK_SEL` as `port + 1` in bits 31:28;
//! * a combo PHY port's PLL and clock gate are in `DPCLKA_CFGCR0`, and the
//!   combo PLLs' dividers in `DPLL_CFGCR0/1` (a DCO in 15-bit fractions of a
//!   reference that is halved when it is 38.4 MHz).
//!
//! Every value the tests assert is in the box's GOP capture
//! (`docs/compat/alderlake/gpu/lazyos/`, decoded in `01-dbgd-capture.md`).

use alloc::format;

use crate::adopt::{running_pipes, ActivePipe, Adopted, Display, PortClock, RegFile, PIPE_NAMES};
use crate::mmio::Mmio;
use crate::regs::{self, DdiFunc, DdiMode};

/// Pipes and transcoders A to D.
pub const PIPES: u32 = 4;

pub const DSSM: u32 = 0x5_1004;
pub const CDCLK_PLL_ENABLE: u32 = 0x4_6070;
/// `DPLL_ENABLE(n)` for the combo PLLs 0 and 1; the Thunderbolt PLL's is at
/// 0x46020.
pub const DPLL_ENABLE: [u32; 2] = [0x4_6010, 0x4_6014];
pub const TBT_PLL_ENABLE: u32 = 0x4_6020;
pub const DPCLKA_CFGCR0: u32 = 0x16_4280;
/// `DPLL_CFGCR0(n)`; `CFGCR1` is the next dword.
pub const DPLL_CFGCR0: [u32; 2] = [0x16_4284, 0x16_428C];

/// `PWR_WELL_CTL*` state bits by well.
pub mod pwr_well {
    pub const PW1_STATE: u32 = 1 << 0;
    pub const PW2_STATE: u32 = 1 << 2;
    /// PW_A .. PW_D (indices 5 to 8): the well of pipe `n`.
    pub const fn pipe_state(pipe: u32) -> u32 {
        1 << (2 * (5 + pipe))
    }
}

/// `*_PLL_ENABLE`: enable (31) and lock (30).
const PLL_ENABLED_LOCKED: u32 = 3 << 30;

/// The DDI ports read: the combo PHY ports A and B and Type-C TC1 to TC4
/// (ports 3 to 6; `DDI_BUF_CTL` keeps the 0x100 stride).
const DDI_PORTS: [(u32, &str); 6] = [
    (0, "A"),
    (1, "B"),
    (3, "TC1"),
    (4, "TC2"),
    (5, "TC3"),
    (6, "TC4"),
];

/// Read the display state: power wells, the clocks (CDCLK, the combo and
/// Thunderbolt PLLs, the port clock routing), every pipe whose well is up
/// and the DDI buffers. The audio block and the Type-C PHYs are not read.
pub fn read(mmio: &mut impl Mmio) -> RegFile {
    let mut file = RegFile::default();
    file.add(mmio, "PWR_WELL_BIOS", regs::PWR_WELL_CTL_BIOS, true);
    file.add(mmio, "PWR_WELL_DRIVER", regs::PWR_WELL_CTL_DRIVER, true);
    file.add(mmio, "DC_STATE_EN", regs::DC_STATE_EN, true);
    file.add(mmio, "CDCLK_CTL", regs::CDCLK_CTL, true);
    if file.looks_dead() {
        return file;
    }
    file.add(mmio, "CDCLK_PLL_ENABLE", CDCLK_PLL_ENABLE, true);
    file.add(mmio, "DSSM", DSSM, true);
    for (pll, &offset) in DPLL_ENABLE.iter().enumerate() {
        file.add(mmio, &format!("DPLL{pll}_ENABLE"), offset, true);
    }
    file.add(mmio, "TBT_PLL_ENABLE", TBT_PLL_ENABLE, true);
    file.add(mmio, "DPCLKA_CFGCR0", DPCLKA_CFGCR0, true);
    for (pll, &offset) in DPLL_CFGCR0.iter().enumerate() {
        file.add(mmio, &format!("DPLL{pll}_CFGCR0"), offset, true);
        file.add(mmio, &format!("DPLL{pll}_CFGCR1"), offset + 4, true);
    }
    // A block behind a well that is down may not answer cleanly: each pipe
    // is read only while PW1 and its own well are up, the Type-C ports only
    // with PW2.
    let well = file.get(regs::PWR_WELL_CTL_DRIVER);
    let pw1 = well & pwr_well::PW1_STATE != 0;
    let pw2 = well & pwr_well::PW2_STATE != 0;
    for pipe in 0..PIPES {
        if pw1 && well & pwr_well::pipe_state(pipe) != 0 {
            file.read_pipe(mmio, pipe);
        } else {
            file.gated
                .push(format!("PIPE_{}", PIPE_NAMES[pipe as usize]));
        }
    }
    for (port, name) in DDI_PORTS {
        let name = format!("DDI_BUF_CTL_{name}");
        if if port < 2 { pw1 } else { pw2 } {
            let offset = regs::DDI_BUF_CTL_A + port * regs::DDI_BUF_STRIDE;
            file.add(mmio, &name, offset, true);
        } else {
            file.gated.push(name);
        }
    }
    file
}

/// Decode a register file [`read`] produced.
pub fn decode(file: &RegFile) -> Adopted {
    let (pipes_enabled, active) = running_pipes(file, PIPES, |pipe| decode_pipe(file, pipe));
    let well = file.get(regs::PWR_WELL_CTL_DRIVER);
    Adopted {
        display: Display::D13,
        active,
        pipes_enabled,
        edp_enabled: false,
        cdclk_khz: cdclk_khz(
            file.get(regs::CDCLK_CTL),
            file.get(CDCLK_PLL_ENABLE),
            file.get(DSSM),
        ),
        cdclk_pll_locked: file.get(CDCLK_PLL_ENABLE) & PLL_ENABLED_LOCKED == PLL_ENABLED_LOCKED,
        power_well_1: well & pwr_well::PW1_STATE != 0,
        power_well_2: well & pwr_well::PW2_STATE != 0,
        dc_state_en: file.get(regs::DC_STATE_EN),
    }
}

fn decode_pipe(file: &RegFile, pipe: u32) -> Option<ActivePipe> {
    let raw = file.get(regs::trans_base(pipe) + regs::trans::DDI_FUNC_CTL);
    let mut func = DdiFunc::decode(raw);
    if !func.enabled {
        return None;
    }
    // Gen9's select field (bits 30:28) does not exist here: the port is
    // `port + 1` in bits 30:27, read below.
    func.ddi_select = 0;
    let port = port_plus_one(raw >> 27 & 0xF)
        .or_else(|| port_plus_one(file.get(regs::trans_clk_sel(pipe)) >> 28));
    let clock = port_clock(file, port, func);
    Some(ActivePipe::decode(file, pipe, func, port, clock))
}

/// A `port + 1` field: `None` for 0 ("no port").
fn port_plus_one(field: u32) -> Option<u8> {
    field.checked_sub(1).map(|port| port as u8)
}

/// The PLL, gate and pixel clock of `port`. Only the combo PHY ports A and
/// B are decoded; a Type-C port's clock (the TBT or MG PLLs) is left
/// unknown. The pixel clock is derived only on a TMDS link (HDMI, DVI): on
/// DisplayPort the PLL runs the link rate, which says nothing of the pixel
/// clock without the link M/N values.
fn port_clock(file: &RegFile, port: Option<u8>, func: DdiFunc) -> PortClock {
    let Some(port) = port.map(u32::from).filter(|&port| port < 2) else {
        return PortClock::default();
    };
    let routing = file.get(DPCLKA_CFGCR0);
    // Clock select of combo PHY `n` in bits `2n+1:2n` (0 = DPLL0, 1 =
    // DPLL1), its gate in bit `10 + n`.
    let pll = match routing >> (2 * port) & 3 {
        select @ (0 | 1) => select as usize,
        _ => return PortClock::default(),
    };
    let pll_ready = file.get(DPLL_ENABLE[pll]) & PLL_ENABLED_LOCKED == PLL_ENABLED_LOCKED;
    let cfgcr0 = file.get(DPLL_CFGCR0[pll]);
    let cfgcr1 = file.get(DPLL_CFGCR0[pll] + 4);
    let tmds = matches!(func.mode, DdiMode::Hdmi | DdiMode::Dvi);
    let bpc = func.bits_per_component.unwrap_or(8).max(1);
    let pixel_clock_khz = (pll_ready && tmds)
        .then(|| combo_port_clock_khz(cfgcr0, cfgcr1, ref_khz(file.get(DSSM))))
        .flatten()
        // HDMI deep colour runs the link at bpc/8 of the pixel clock.
        .map(|port_khz| port_khz * 8 / u32::from(bpc));
    PortClock {
        pll: Some(pll as u8),
        pll_ready,
        ddi_clock_off: routing & (1 << (10 + port)) != 0,
        pixel_clock_khz,
    }
}

/// The display reference clock `DSSM` straps (bits 31:29: 0 = 24 MHz,
/// 1 = 19.2 MHz, 2 = 38.4 MHz), in kHz; `None` for a reserved value.
pub fn ref_khz(dssm: u32) -> Option<u32> {
    match dssm >> 29 {
        0 => Some(24_000),
        1 => Some(19_200),
        2 => Some(38_400),
        _ => None,
    }
}

/// CDCLK in kHz: the CDCLK PLL's ratio (bits 7:0 of its enable register)
/// times the reference, over `CDCLK_CTL`'s CD2X divider (bits 23:22: 1, 1.5,
/// 2, 4) and two. While the PLL is off, or the reference unknown, the
/// decimal field `CDCLK_CTL` also carries (`(MHz - 1) * 2`, bits 10:0).
pub fn cdclk_khz(ctl: u32, pll_enable: u32, dssm: u32) -> u32 {
    let ratio = pll_enable & 0xFF;
    match ref_khz(dssm) {
        Some(reference) if pll_enable & PLL_ENABLED_LOCKED == PLL_ENABLED_LOCKED && ratio != 0 => {
            // The divider in halves: 1 -> 2, 1.5 -> 3, 2 -> 4, 4 -> 8.
            let halves = [2, 3, 4, 8][(ctl >> 22 & 3) as usize];
            ratio * reference / halves
        }
        _ => regs::cdclk_khz(ctl),
    }
}

/// A combo PLL's HDMI port clock in kHz, rounded, from `DPLL_CFGCR0`
/// (DCO integer bits 9:0, fraction bits 24:10 in 1/2^15) and `DPLL_CFGCR1`
/// (P bits 5:2 as 1/2/4/8 for 2/3/5/7, K bits 8:6 as 1/2/4 for 1/2/3, Q
/// bits 17:10 when bit 9 is set): `DCO / (P Q K) / 5`, the DCO counted in a
/// reference halved from 38.4 MHz. `None` for a reserved divider.
///
/// With that 38.4 MHz reference the fraction is programmed at half its
/// value and read back doubled (i915's Wa_22010492432,
/// `ehl_combo_pll_div_frac_wa_needed`, which covers display 12 and ADL-P):
/// the box's DPLL1 is 241.5 MHz with it, and its frame counter agrees.
pub fn combo_port_clock_khz(cfgcr0: u32, cfgcr1: u32, reference: Option<u32>) -> Option<u32> {
    let (reference, fraction_scale) = match reference? {
        38_400 => (19_200, 2),
        other => (other, 1),
    };
    let p = match cfgcr1 >> 2 & 0xF {
        1 => 2,
        2 => 3,
        4 => 5,
        8 => 7,
        _ => return None,
    };
    let k = match cfgcr1 >> 6 & 7 {
        1 => 1,
        2 => 2,
        4 => 3,
        _ => return None,
    };
    let q = if cfgcr1 & (1 << 9) != 0 {
        match cfgcr1 >> 10 & 0xFF {
            0 => return None,
            q => u64::from(q),
        }
    } else {
        1
    };
    let integer = u64::from(cfgcr0 & 0x3FF);
    let fraction = u64::from(cfgcr0 >> 10 & 0x7FFF) * fraction_scale;
    // In units of 1/2^15 kHz, so the division rounds once.
    let dco = u64::from(reference) * ((integer << 15) + fraction);
    let divider = ((p * k * 5) as u64 * q) << 15;
    let khz = (dco + divider / 2) / divider;
    u32::try_from(khz).ok().filter(|&khz| khz != 0)
}
