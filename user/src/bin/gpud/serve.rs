//! `os.lazy.gpu.v1` (`idl/gpu.midl`), served from the snapshot taken at
//! start. Read-only: the one method reports what was adopted.

use alloc::vec;
use gen9gfx::adopt::{Adopted, FbMap};
use gen9gfx::regs::{plane_ctl, DdiMode};
use user::messenger::gpu::{self as api, wire};
use user::messenger::services;
use user::messenger::{self, errno, registry, Error as MsgError, Message, Parcel};
use user::sys;

/// Longest park between wake-ups (100 Hz ticks) of the service loop.
const IDLE_TICKS: u64 = 6000;

/// The PCI identity words of the snapshot.
pub(super) struct Facts {
    pub(super) device: u16,
    pub(super) revision: u8,
}

/// The wire state for what was adopted.
pub(super) fn state(facts: &Facts, adopted: &Adopted, fb: Option<&FbMap>) -> wire::State {
    let mut out = wire::State {
        adopted: adopted.active.is_some(),
        device: u32::from(facts.device),
        revision: u32::from(facts.revision),
        cdclk_khz: adopted.cdclk_khz,
        power_well_1: adopted.power_well_1,
        power_well_2: adopted.power_well_2,
        pipes_enabled: u32::from(adopted.pipes_enabled),
        pipe: 0,
        ddi_port: u32::MAX,
        link: wire::LINK_MODE_NONE,
        bits_per_component: 0,
        hsync_positive: false,
        vsync_positive: false,
        pixel_clock_khz: 0,
        refresh_millihz: 0,
        horizontal: wire::Axis::default(),
        vertical: wire::Axis::default(),
        plane: wire::Plane::default(),
        framebuffer: wire::FbMap::default(),
        audio_capable: false,
    };
    let Some(pipe) = adopted.active else {
        return out;
    };
    out.pipe = u32::from(pipe.pipe);
    out.ddi_port = pipe.port.map_or(u32::MAX, u32::from);
    out.link = match pipe.func.mode {
        DdiMode::Hdmi => wire::LINK_MODE_HDMI,
        DdiMode::Dvi => wire::LINK_MODE_DVI,
        DdiMode::DpSst => wire::LINK_MODE_DP_SST,
        DdiMode::DpMst => wire::LINK_MODE_DP_MST,
        _ => wire::LINK_MODE_OTHER,
    };
    out.bits_per_component = pipe.func.bits_per_component.map_or(0, u32::from);
    out.hsync_positive = pipe.func.hsync_positive;
    out.vsync_positive = pipe.func.vsync_positive;
    out.pixel_clock_khz = pipe.pixel_clock_khz.unwrap_or(0);
    out.refresh_millihz = pipe.refresh_millihz().unwrap_or(0);
    out.horizontal = axis(&pipe.h);
    out.vertical = axis(&pipe.v);
    out.audio_capable = pipe.hdmi_link();
    let plane = pipe.plane;
    out.plane = wire::Plane {
        enabled: plane.enabled,
        format: match plane.format {
            plane_ctl::Format::Xrgb8888 => 4,
            plane_ctl::Format::Other(code) => u32::from(code),
        },
        tiling: match plane.tiling {
            plane_ctl::Tiling::Linear => wire::TILING_LINEAR,
            plane_ctl::Tiling::X => wire::TILING_X,
            plane_ctl::Tiling::Y => wire::TILING_Y,
            plane_ctl::Tiling::Yf => wire::TILING_YF,
            plane_ctl::Tiling::Other(_) => wire::TILING_OTHER,
        },
        stride_bytes: plane.stride_bytes,
        width: plane.width,
        height: plane.height,
        surface: plane.surf,
        surface_live: plane.surf_live,
    };
    if let Some(map) = fb {
        out.framebuffer = wire::FbMap {
            pages: map.pages,
            present: map.present,
            phys_base: map.first,
            contiguous: map.contiguous,
        };
    }
    out
}

fn axis(axis: &gen9gfx::adopt::Axis) -> wire::Axis {
    wire::Axis {
        active: axis.active,
        total: axis.total,
        blank_start: axis.blank_start,
        blank_end: axis.blank_end,
        sync_start: axis.sync_start,
        sync_end: axis.sync_end,
    }
}

/// Register `os.lazy.gpu` and serve forever (an error before the first
/// request is returned: the caller parks).
pub(super) fn run(snapshot: wire::State) -> Result<core::convert::Infallible, &'static str> {
    let (published, server) = messenger::create_pair().map_err(|_| "no endpoint")?;
    registry::register(api::NAME, &published, &[api::INTERFACE], 0)
        .map_err(|_| "cannot register os.lazy.gpu")?;
    user::trace::line!(
        user::trace::Channel::Gpu,
        "READY name={} adopted={}",
        api::NAME,
        u8::from(snapshot.adopted)
    );
    services::init::notify_ready();
    let mut buffer = vec![0u8; messenger::DEFAULT_BUFFER];
    loop {
        match server.recv_with(&mut buffer, Some(sys::clock() + IDLE_TICKS)) {
            Ok(message) => {
                let reply = dispatch(&message, &snapshot).unwrap_or_else(|error| {
                    services::error_reply(message.interface_id(), message.method(), error)
                });
                if let Some(txn) = message.txn {
                    let _ = server.reply_or_drop(txn, &reply);
                }
            }
            Err(MsgError::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(_) => sys::nap(),
        }
    }
}

fn dispatch(message: &Message, snapshot: &wire::State) -> Result<Parcel, MsgError> {
    if message.interface_id() != api::INTERFACE {
        return Err(MsgError::Errno(-errno::EINVAL));
    }
    match message.method() {
        wire::METHOD_GETSTATE => {
            let body = wire::encode_get_state_reply(&wire::GetStateReply {
                state: snapshot.clone(),
            })
            .map_err(MsgError::Parcel)?;
            Ok(api::parcel(wire::METHOD_GETSTATE, body))
        }
        _ => Err(MsgError::Errno(-errno::EINVAL)),
    }
}
