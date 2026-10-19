//! The adopted pipe's beam (`intelgfx::vblank`): the refresh rate measured
//! from the frame counter once at start (G1's "vblank ticks at the monitor's
//! rate", `GPU:VBLANK`), the FIFO underrun state (`GPU:UNDERRUN`), and the
//! live read `GetBeam` answers with. Every access is a read through the
//! read-only BAR.

use intelgfx::adopt::ActivePipe;
use intelgfx::regs::DdiMode;
use intelgfx::vblank::{self, Beam, PipeIrq, Underrun};
use user::messenger::gpu::wire;
use user::sys;
use user::trace::{self, Channel};

use crate::bar::ReadOnlyBar;

/// How long the start-up measurement watches the counter: a second is 60
/// frames, and the scan lines make it good to a line.
const MEASURE_NS: u64 = 1_000_000_000;
/// The measured rate agrees with the timing registers within this many parts
/// per million (the timer's granularity is far finer than a line).
const TOLERANCE_PPM: u32 = 2000;

/// A live view of one pipe's beam.
pub(super) struct Watch {
    regs: ReadOnlyBar,
    pipe: u32,
    vtotal: u32,
    measured_millihz: u32,
}

impl Watch {
    /// Measure the refresh rate of `pipe` and report it with the underrun
    /// state; the watch then answers `GetBeam`.
    pub(super) fn start(mut regs: ReadOnlyBar, pipe: &ActivePipe) -> Watch {
        let index = u32::from(pipe.pipe);
        let vtotal = pipe.v.total;
        // The counter moves at the start of the vertical blank, where raw
        // `PIPEDSL` still reads 2 lines short on a TMDS link and 1 on
        // DisplayPort (i915's `intel_crtc_scanline_offset`, DDI platforms).
        // On display 13 the vertical blank also starts `TRANS_SET_CONTEXT_
        // LATENCY` lines after `TRANS_VBLANK`'s start; both boxes' firmware
        // leaves it 0, which is all this reads.
        let lag = if pipe.hdmi_link() || pipe.func.mode == DdiMode::Dvi {
            2
        } else {
            1
        };
        let count_line = pipe.v.blank_start.saturating_sub(lag);
        let (first, t0) = (Beam::read(&mut regs, index), sys::monotonic_ns());
        let _ = sys::sleep_ns(MEASURE_NS);
        let (last, t1) = (Beam::read(&mut regs, index), sys::monotonic_ns());
        let measured =
            vblank::measured_millihz(first, last, t1.saturating_sub(t0), vtotal, count_line);
        let expected = pipe.refresh_millihz().unwrap_or(0);
        let frames = last.frame.wrapping_sub(first.frame);
        match measured {
            Some(millihz) if vblank::agrees(millihz, expected, TOLERANCE_PPM) => trace::pass!(
                Channel::Gpu,
                "VBLANK",
                "measured_mhz={millihz} expected_mhz={expected} frames={frames} line={}",
                last.line
            ),
            _ => trace::fail!(
                Channel::Gpu,
                "VBLANK",
                "measured_mhz={} expected_mhz={expected} frames={frames} first={}:{} last={}:{} \
                 ns={}",
                measured.unwrap_or(0),
                first.frame,
                first.line,
                last.frame,
                last.line,
                t1.saturating_sub(t0)
            ),
        }
        let irq = PipeIrq::read(&mut regs, index);
        trace::line!(
            Channel::Gpu,
            "UNDERRUN pipe={} state={} isr={:#010x} imr={:#010x} iir={:#010x} ier={:#010x}",
            index,
            underrun_name(irq.underrun()),
            irq.isr,
            irq.imr,
            irq.iir,
            irq.ier
        );
        Watch {
            regs,
            pipe: index,
            vtotal,
            // Only a rate the timing registers confirm is handed on (clients pace their frames to it).
            measured_millihz: measured
                .filter(|&millihz| vblank::agrees(millihz, expected, TOLERANCE_PPM))
                .unwrap_or(0),
        }
    }

    /// The beam now, for `GetBeam`.
    pub(super) fn read(&mut self) -> wire::Beam {
        let beam = Beam::read(&mut self.regs, self.pipe);
        let at_ns = sys::monotonic_ns();
        let underrun = match PipeIrq::read(&mut self.regs, self.pipe).underrun() {
            Underrun::None => wire::UNDERRUN_NONE,
            Underrun::Latched => wire::UNDERRUN_LATCHED,
            Underrun::Unobserved => wire::UNDERRUN_UNOBSERVED,
        };
        wire::Beam {
            frame: beam.frame,
            line: beam.line,
            vtotal: self.vtotal,
            at_ns,
            measured_millihz: self.measured_millihz,
            underrun,
        }
    }
}

fn underrun_name(state: Underrun) -> &'static str {
    match state {
        Underrun::None => "none",
        Underrun::Latched => "latched",
        Underrun::Unobserved => "masked",
    }
}
