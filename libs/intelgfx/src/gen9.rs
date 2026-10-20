//! Gen9's half of [`crate::adopt`]: which registers the HD Graphics 620
//! (`8086:5916`) is read for, and how its clocks, power wells and DDI routing
//! decode. Tested against the box's `intel_reg dump`
//! (`docs/compat/kabylake/gpu/linux/`, `tests_adopt`).

use alloc::format;
use alloc::string::String;

use crate::adopt::{running_pipes, ActivePipe, Adopted, Display, PortClock, RegFile, PIPE_NAMES};
use crate::mmio::Mmio;
use crate::regs::{self, DdiFunc};

/// Pipes (and transcoders A to C) a Gen9 GT2 has.
pub const PIPES: usize = 3;
/// DDI ports A to E.
pub const DDI_PORTS: usize = 5;

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

const DDI_NAMES: [&str; DDI_PORTS] = ["A", "B", "C", "D", "E"];

/// Read the display state of the whole device: power wells, CDCLK, the
/// DPLLs, the three pipes with their transcoder, DDI function and first
/// plane, the DDI buffers and the audio block (the last two with offsets
/// still to confirm), and the forcewake registers.
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
    // A register behind a power well that is down may not answer cleanly,
    // so the blocks below are read only while their well is up: pipe A, the
    // eDP transcoder and DDI A are in well 1, pipes B and C, DDI B to E and
    // the audio block in well 2. A bus that answers all ones everywhere is
    // not read further at all.
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

/// Decode a register file [`read`] produced.
pub fn decode(file: &RegFile) -> Adopted {
    let (pipes_enabled, active) = running_pipes(file, PIPES as u32, |pipe| decode_pipe(file, pipe));
    let lcpll1 = file.get(regs::LCPLL1_CTL);
    let well = file.get(regs::PWR_WELL_CTL_DRIVER);
    Adopted {
        display: Display::Gen9,
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
    Some(ActivePipe::decode(
        file,
        pipe,
        func,
        port,
        port_clock(file, port),
    ))
}

fn port_clock(file: &RegFile, port: Option<u8>) -> PortClock {
    let ctrl1 = file.get(regs::DPLL_CTRL1);
    let ctrl2 = file.get(regs::DPLL_CTRL2);
    let pll = port
        .and_then(|port| regs::dpll_ctrl2_ddi_pll(ctrl2, u32::from(port)))
        .map(|pll| pll as u8);
    let hdmi_mode = pll.is_some_and(|pll| regs::dpll_ctrl1_hdmi_mode(ctrl1, u32::from(pll)));
    // `DDI_CLK_OFF` of port `n` is bit `3 n + 15`; ports are A to E only.
    let ddi_clock_off = port
        .filter(|&port| usize::from(port) < DDI_PORTS)
        .is_some_and(|port| ctrl2 & (1 << (u32::from(port) * 3 + 15)) != 0);
    // Only DPLL1 to 3 have dividers; DPLL0 is the CDCLK PLL.
    let pixel_clock_khz = pll.filter(|_| hdmi_mode).and_then(|pll| match pll {
        1..=3 => {
            let base = regs::DPLL1_CFGCR1 + (u32::from(pll) - 1) * regs::DPLL_CFGCR_STRIDE;
            regs::dpll_hdmi_clock_khz(file.get(base), file.get(base + 4))
        }
        _ => None,
    });
    PortClock {
        pll,
        pll_ready: hdmi_mode,
        ddi_clock_off,
        pixel_clock_khz,
    }
}
