//! Stage G2, the display half of HDMI audio (docs/kabylake-gpu-plan.md,
//! section 4): the only place `gpud` writes a register.
//!
//! Off unless the service was started with `audio=1` (the image's
//! `LAZYOS_GPUD_ARGS`, `run_demo.py --gpud-audio`). It opens the BAR's audio
//! grant (`bar.rs`: the audio block of transcoders A to C and nothing else),
//! derives the request from what was adopted and the sink's EDID
//! (`intelgfx::audio::prepare`), and runs `intelgfx::audio::enable`. It
//! refuses a link that is not already in HDMI mode, so it is never a mode
//! set and needs no revert timer: what it changes is the audio block's
//! ELD, output enable and clock regeneration, which the screen does not show.
//!
//! The EDID comes from `edid=<path>` (a file the image carries) until a DDC
//! reader exists (GMBUS, not built); with neither, it says so and does
//! nothing. The `N` it programs is for 48 kHz, the rate the box's sink and
//! most streams use: following the stream's rate is the next step, and a
//! 32 or 44.1 kHz stream will play at the wrong speed until then.
//!
//! Evidence: `GPU:AUDIO:PASS`, `GPU:AUDIO:SKIP:<why>`, `GPU:AUDIO:FAIL`.

use alloc::format;
use intelgfx::adopt::Adopted;
use intelgfx::args::Args;
use intelgfx::audio::{self, PrepareError};
use intelgfx::hdmi_audio::Rate;
use user::trace::{self, Channel};

use crate::bar::Bar;

/// An EDID is 128 bytes per block; a monitor has a handful of extensions.
const EDID_LIMIT: usize = 1024;

/// Enable the display side of HDMI audio for the adopted pipe, if asked to.
pub(super) fn run(args: &Args, regs: &mut Bar, adopted: &Adopted) {
    if !args.audio {
        return;
    }
    let Some(path) = args.edid.as_deref() else {
        return skip(
            "no-edid",
            "start with edid=<path>: no DDC reader is built yet",
        );
    };
    let edid = match user::files::read_up_to(path, EDID_LIMIT) {
        Ok(bytes) => bytes,
        Err(errno) => return skip("edid-read", &format!("{path} errno={errno}")),
    };
    let mut prepared = match audio::prepare(adopted, &edid, Rate::Hz48000) {
        Ok(prepared) => prepared,
        Err(why) => return skip(why_name(why), &format!("{why:?}")),
    };
    // Bring-up aid, off in every normal build: `LAZYOS_GPUD_FORCE_N=<n>` forces
    // `N` instead of the hardware's automatic one, to compare on the box.
    if let Some(n) = option_env!("LAZYOS_GPUD_FORCE_N").and_then(|n| n.parse().ok()) {
        prepared.n = Some(n);
    }
    let outcome = regs.with_audio_writes(|regs| audio::enable(regs, &prepared.request()));
    match outcome {
        Ok(done) => trace::pass!(
            Channel::Gpu,
            "AUDIO",
            "transcoder={} tmds_khz={} n={:?} eld_dwords={} aud_config={:#010x} pin_buf={:#010x} writes={}",
            prepared.transcoder,
            prepared.tmds_khz,
            prepared.n,
            done.eld_dwords,
            done.config,
            done.pin_buf,
            regs.writes()
        ),
        Err(why) => trace::fail!(Channel::Gpu, "AUDIO", "{why:?} writes={}", regs.writes()),
    }
}

fn why_name(why: PrepareError) -> &'static str {
    match why {
        PrepareError::Unsupported(_) => "unsupported-display",
        PrepareError::NoPipe => "no-pipe",
        PrepareError::NotHdmi => "not-hdmi",
        PrepareError::NoClock => "no-clock",
        PrepareError::BadEdid(_) => "bad-edid",
        PrepareError::NoAudio => "no-audio",
        PrepareError::NoLpcm => "no-lpcm",
    }
}

fn skip(why: &str, detail: &str) {
    trace::line!(Channel::Gpu, "AUDIO:SKIP:{why} {detail}");
}
