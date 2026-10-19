//! What a repaint costs the compositor, per megapixel (`XUID:COMPOSE`): the time spent composing the
//! damage into the screen buffer and the time the `present` syscall took to copy it to the
//! framebuffer, summed over [`PERIOD_NS`] and printed once per period with any pixels in it.
//!
//! The kernel's `PERF:present_mpx` (a `LAZYOS_PERF=1` image only) measures the copy from inside; this
//! line is always on and also covers composing, the user-space half nothing else times. Together
//! they say where a scrolling frame's compositor time goes (docs/kabylake-gpu-plan.md 11.3: on the
//! Kaby Lake box the copy lands in a write-combining aperture, so the two halves can differ a lot).

use user::sys;
use user::trace::{self, Channel};

/// How often the totals are printed and reset.
const PERIOD_NS: u64 = 5_000_000_000;

/// Totals of one period.
#[derive(Default)]
pub(super) struct Meter {
    started_ns: u64,
    repaints: u32,
    pixels: u64,
    compose_ns: u64,
    present_ns: u64,
    worst_ns: u64,
}

impl Meter {
    /// One repaint of `pixels` pixels: composing took `compose_ns`, the present `present_ns`.
    pub(super) fn record(&mut self, pixels: u64, compose_ns: u64, present_ns: u64) {
        self.repaints = self.repaints.saturating_add(1);
        self.pixels = self.pixels.saturating_add(pixels);
        self.compose_ns = self.compose_ns.saturating_add(compose_ns);
        self.present_ns = self.present_ns.saturating_add(present_ns);
        self.worst_ns = self.worst_ns.max(compose_ns.saturating_add(present_ns));
        self.report_if_due(sys::monotonic_ns());
    }

    fn report_if_due(&mut self, now_ns: u64) {
        if self.started_ns == 0 {
            self.started_ns = now_ns;
        }
        if now_ns.saturating_sub(self.started_ns) < PERIOD_NS {
            return;
        }
        self.report();
        *self = Meter {
            started_ns: now_ns,
            ..Meter::default()
        };
    }

    /// The period's line (none when it composed nothing). Formatted by the trace call itself: the
    /// user heap is a bump allocator, so a `String` here would leak one per period.
    fn report(&self) {
        if self.pixels == 0 {
            return;
        }
        let per_mpx = |ns: u64| (u128::from(ns) * 1000 / u128::from(self.pixels)) as u64;
        trace::line!(
            Channel::Xuid,
            "COMPOSE repaints={} kpx={} compose_us={} present_us={} compose_us_per_mpx={} \
             present_us_per_mpx={} worst_us={}",
            self.repaints,
            self.pixels / 1000,
            self.compose_ns / 1000,
            self.present_ns / 1000,
            per_mpx(self.compose_ns),
            per_mpx(self.present_ns),
            self.worst_ns / 1000
        );
    }
}
