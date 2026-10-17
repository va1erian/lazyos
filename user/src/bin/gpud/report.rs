//! `gpud`'s evidence lines (`GPU:*`), through the trace API: what a person
//! reads over `dbgd` (`dbgctl log --source programs`) to learn what the
//! firmware left. One line per fact; the raw register values are printed too,
//! so a decoder that turns out wrong can be corrected from the log alone.
//!
//! Nothing secret is here: register values, addresses of the display's own
//! state.

use gen9gfx::adopt::{ActivePipe, Adopted, FbMap, RegFile};
use gen9gfx::regs::{plane_ctl, DdiMode};
use user::trace::{self, Channel};

use crate::device::{Command, Identity};

/// `GPU:PCI`: the function and its command register before and after the
/// claim's quiesce was undone.
pub(super) fn pci(id: &Identity, command: &Command) {
    trace::line!(
        Channel::Gpu,
        "PCI rev={:#04x} subsystem={:04x}:{:04x} gmch_ctrl={:#06x} bdsm={:#010x} asls={:#010x} \
         cmd_after_claim={:#06x} cmd_restored={:#06x} bus_master_error={}",
        id.revision,
        id.subsystem_vendor,
        id.subsystem_device,
        id.gmch_ctrl,
        id.bdsm,
        id.asls,
        command.after_claim,
        command.restored,
        command.bus_master_error.unwrap_or(0)
    );
}

/// `GPU:REG`: every register that was read, in read order, and `GPU:GATED`
/// for the blocks left unread because their power well was down.
pub(super) fn registers(file: &RegFile) {
    if !file.gated.is_empty() {
        trace::line!(Channel::Gpu, "GATED blocks={}", file.gated.join(","));
    }
    for reg in &file.regs {
        trace::line!(
            Channel::Gpu,
            "REG {} off={:#07x} val={:#010x} offset_confirmed={}",
            reg.name,
            reg.offset,
            reg.value,
            u8::from(reg.confirmed)
        );
    }
}

fn link_name(pipe: &ActivePipe) -> &'static str {
    match pipe.func.mode {
        DdiMode::Hdmi => "hdmi",
        DdiMode::Dvi => "dvi",
        DdiMode::DpSst => "dp-sst",
        DdiMode::DpMst => "dp-mst",
        DdiMode::Fdi => "fdi",
        DdiMode::Reserved(_) => "reserved",
    }
}

fn tiling_name(tiling: plane_ctl::Tiling) -> &'static str {
    match tiling {
        plane_ctl::Tiling::Linear => "linear",
        plane_ctl::Tiling::X => "x",
        plane_ctl::Tiling::Y => "y",
        plane_ctl::Tiling::Yf => "yf",
        plane_ctl::Tiling::Other(_) => "other",
    }
}

/// `GPU:POWER`, `GPU:CDCLK`, then the pipe's lines or `GPU:ADOPT:NONE`.
pub(super) fn adopted(state: &Adopted, fb: Option<&FbMap>) {
    trace::line!(
        Channel::Gpu,
        "POWER pw1={} pw2={} dc_state_en={:#x}",
        u8::from(state.power_well_1),
        u8::from(state.power_well_2),
        state.dc_state_en
    );
    trace::line!(
        Channel::Gpu,
        "CDCLK khz={} lcpll1_locked={}",
        state.cdclk_khz,
        u8::from(state.cdclk_pll_locked)
    );
    let Some(pipe) = state.active else {
        trace::line!(
            Channel::Gpu,
            "ADOPT:NONE pipes_enabled={:#05b} edp={}",
            state.pipes_enabled,
            u8::from(state.edp_enabled)
        );
        return;
    };
    let port = pipe.port.map_or(-1, i32::from);
    trace::line!(
        Channel::Gpu,
        "PIPE pipe={} port={} link={} bpc={} hsync={} vsync={} pll={} pll_hdmi={} ddi_clock_off={} \
         pipes_enabled={:#05b}",
        pipe.pipe,
        port,
        link_name(&pipe),
        pipe.func.bits_per_component.unwrap_or(0),
        char::from(b"-+"[usize::from(pipe.func.hsync_positive)]),
        char::from(b"-+"[usize::from(pipe.func.vsync_positive)]),
        pipe.pll.map_or(-1, i32::from),
        u8::from(pipe.pll_hdmi_mode),
        u8::from(pipe.ddi_clock_off),
        state.pipes_enabled
    );
    trace::line!(
        Channel::Gpu,
        "MODE {}x{} h_total={} h_sync={}-{} v_total={} v_sync={}-{} pixel_khz={} refresh_mhz={} \
         source={}",
        pipe.h.active,
        pipe.v.active,
        pipe.h.total,
        pipe.h.sync_start,
        pipe.h.sync_end,
        pipe.v.total,
        pipe.v.sync_start,
        pipe.v.sync_end,
        pipe.pixel_clock_khz.unwrap_or(0),
        pipe.refresh_millihz().unwrap_or(0),
        pipe.source
            .is_some_and(|(w, h)| (w, h) == (pipe.h.active, pipe.v.active))
    );
    trace::line!(
        Channel::Gpu,
        "INFOFRAMES gcp={} avi={} spd={} dip_ctl={:#010x}",
        u8::from(pipe.dip_gcp()),
        u8::from(pipe.dip_avi()),
        u8::from(pipe.dip_spd()),
        pipe.dip
    );
    let plane = pipe.plane;
    trace::line!(
        Channel::Gpu,
        "PLANE enabled={} format={:?} tiling={} stride={} {}x{} at={},{} surf={:#010x} live={:#010x}",
        u8::from(plane.enabled),
        plane.format,
        tiling_name(plane.tiling),
        plane.stride_bytes,
        plane.width,
        plane.height,
        plane.x,
        plane.y,
        plane.surf,
        plane.surf_live
    );
    if let Some(map) = fb {
        trace::line!(
            Channel::Gpu,
            "FB ggtt={:#010x} bytes={} pages={} present={} first={:#x} last={:#x} contiguous={}",
            plane.surf,
            plane.bytes(),
            map.pages,
            map.present,
            map.first,
            map.last,
            u8::from(map.contiguous)
        );
    }
    trace::line!(
        Channel::Gpu,
        "ADOPT:PASS pipe={} port={} {}x{} link={} pixel_khz={} cdclk_khz={} audio_capable={}",
        pipe.pipe,
        port,
        pipe.h.active,
        pipe.v.active,
        link_name(&pipe),
        pipe.pixel_clock_khz.unwrap_or(0),
        state.cdclk_khz,
        u8::from(pipe.hdmi_link())
    );
}
