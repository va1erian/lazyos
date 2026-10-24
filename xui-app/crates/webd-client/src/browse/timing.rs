//! Frames that carry only what changed (`DELTA`: a wheel turn on a tall page comes back as a shift
//! and the rows it uncovered) and how long the pictures take (`LATENCY`: a load to its first frame,
//! a wheel turn to the next frame, a refresh to its whole frame).

use super::steps::{FIRST_LOAD_MS, STEP_MS};
use super::{Browse, Marker, Stage, Which};
use crate::transport::ScopedTransport;

/// Wheel turns timed, alternately down and up.
pub const WHEEL_TURNS: u32 = 20;
/// Refreshes timed.
pub const REFRESHES: u32 = 10;
/// Notches per wheel turn.
const NOTCHES: i32 = 3;
/// A view is quiet when no frame came for this long (a smooth scroll ends), or after the cap.
const QUIET_MS: u64 = 200;
const QUIET_CAP_MS: u64 = 2000;
/// One timed turn gives up after this.
const TURN_MS: u64 = 3000;
/// The delta after the first wheel turn.
const DELTA_MS: u64 = 15_000;

/// What `DELTA` and `LATENCY` measured.
#[derive(Debug, Default)]
pub(super) struct Timing {
    load_ms: Option<u64>,
    /// When the long page was first seen committed.
    committed_at: Option<u64>,
    wheel: Vec<u64>,
    refresh: Vec<u64>,
    lost_wheel: u32,
    lost_refresh: u32,
    turn: u32,
}

/// `min=.. median=.. max=..` of `samples` (sorted here).
pub fn summary(samples: &mut [u64]) -> String {
    samples.sort_unstable();
    match samples {
        [] => "min=- median=- max=-".into(),
        _ => format!(
            "min={} median={} max={}",
            samples[0],
            samples[samples.len() / 2],
            samples[samples.len() - 1]
        ),
    }
}

impl<T: ScopedTransport> Browse<T> {
    /// No frame of view A for [`QUIET_MS`] (or the cap passed since `mark`).
    fn quiet(&self, now: u64) -> bool {
        let last = self.seen.frames.last().map(|f| f.at).unwrap_or(0);
        now.saturating_sub(last.max(self.mark)) >= QUIET_MS
            || now.saturating_sub(self.mark) >= QUIET_CAP_MS
    }

    /// A wheel turn at the middle of view A (`ticks_y < 0` scrolls down), timed from now.
    fn turn_wheel(&mut self, ticks_y: i32, now: u64) {
        let (w, h) = self.a.as_ref().map_or((0, 0), |a| a.size());
        if let Some(a) = self.a.as_mut() {
            a.wheel(w as i32 / 2, h as i32 / 2, 0, ticks_y, 0, 0);
        }
        self.seen.frames.clear();
        self.mark = now;
    }

    pub(super) fn stage_delta(&mut self, now: u64, out: &mut Vec<Marker>) {
        let long = self.url("/long");
        match self.phase {
            0 => {
                self.phase = 1;
                self.timing = Timing::default();
                self.seen.frames.clear();
                self.mark = now;
                if let Err(error) = self.load(Which::A, &long) {
                    self.fail("DELTA", format!("load {long}: {error}"), out);
                    self.next(Stage::Latency, now);
                }
            }
            1 => {
                if self.committed(Which::A) == long && self.timing.committed_at.is_none() {
                    self.timing.committed_at = Some(now);
                }
                if let (None, Some(at)) = (self.timing.load_ms, self.timing.committed_at) {
                    // The first frame since the long page was on screen.
                    let first = self.seen.frames.iter().find(|f| f.at >= at);
                    self.timing.load_ms = first.map(|f| f.at.saturating_sub(self.mark));
                }
                if self.timing.load_ms.is_some() && self.loaded(Which::A, None) {
                    self.phase = 2;
                    self.mark = now;
                } else if self.elapsed(now) > FIRST_LOAD_MS {
                    self.fail("DELTA", format!("timeout loading {long}"), out);
                    self.next(Stage::Latency, now);
                }
            }
            2 if self.quiet(now) => {
                self.turn_wheel(-NOTCHES, now);
                self.phase = 3;
            }
            3 => {
                let shifted = self.seen.frames.iter().find(|f| !f.whole && f.dy != 0);
                if let Some(frame) = shifted.copied() {
                    let detail = format!("dy={} rects={}", frame.dy, frame.rects);
                    self.pass("DELTA", detail, out);
                    self.next(Stage::Latency, now);
                } else if now.saturating_sub(self.mark) > DELTA_MS {
                    let frames = self.seen.frames.len();
                    let whole = self.seen.frames.iter().filter(|f| f.whole).count();
                    let why = format!("no delta with a shift (frames={frames} whole={whole})");
                    self.fail("DELTA", why, out);
                    self.next(Stage::Latency, now);
                }
            }
            _ => {}
        }
    }

    /// Phases: 0 waits for quiet and turns the wheel, 1 times its first frame; 2 waits for quiet
    /// and asks for a refresh, 3 times its whole frame.
    pub(super) fn stage_latency(&mut self, now: u64, out: &mut Vec<Marker>) {
        match self.phase {
            0 if self.quiet(now) => {
                // Down first: the long page is scrolled a little, never back to its top.
                let ticks = if self.timing.turn.is_multiple_of(2) {
                    -NOTCHES
                } else {
                    NOTCHES
                };
                self.turn_wheel(ticks, now);
                self.phase = 1;
            }
            1 => {
                let first = self.seen.frames.first().map(|f| f.at);
                let took = match first {
                    Some(at) => Some(at.saturating_sub(self.mark)),
                    None if now.saturating_sub(self.mark) > TURN_MS => None,
                    None => return,
                };
                match took {
                    Some(ms) => self.timing.wheel.push(ms),
                    None => self.timing.lost_wheel += 1,
                }
                self.timing.turn += 1;
                self.mark = now;
                self.phase = if self.timing.turn >= WHEEL_TURNS {
                    2
                } else {
                    0
                };
                if self.phase == 2 {
                    self.timing.turn = 0;
                }
            }
            2 if self.quiet(now) => {
                if let Some(a) = self.a.as_mut() {
                    a.refresh();
                }
                self.seen.frames.clear();
                self.mark = now;
                self.phase = 3;
            }
            3 => {
                let first = self.seen.frames.iter().find(|f| f.whole).map(|f| f.at);
                match first {
                    Some(at) => self.timing.refresh.push(at.saturating_sub(self.mark)),
                    None if now.saturating_sub(self.mark) > TURN_MS => {
                        self.timing.lost_refresh += 1;
                    }
                    None => return,
                }
                self.timing.turn += 1;
                self.mark = now;
                self.phase = 2;
                if self.timing.turn >= REFRESHES {
                    self.latency_verdict(out);
                    self.next(Stage::Done, now);
                }
            }
            _ if self.elapsed(now) > STEP_MS * 3 => {
                self.fail("LATENCY", "timeout".into(), out);
                self.next(Stage::Done, now);
            }
            _ => {}
        }
    }

    fn latency_verdict(&mut self, out: &mut Vec<Marker>) {
        let timing = &mut self.timing;
        let load = timing
            .load_ms
            .map_or_else(|| "-".to_owned(), |ms| ms.to_string());
        let (wheels, refreshes) = (timing.wheel.len(), timing.refresh.len());
        let detail = format!(
            "load_ms={load} wheel_ms {} n={wheels} lost={} refresh_ms {} n={refreshes} lost={}",
            summary(&mut timing.wheel),
            timing.lost_wheel,
            summary(&mut timing.refresh),
            timing.lost_refresh
        );
        let fine = timing.load_ms.is_some() && wheels > 0 && refreshes > 0;
        match fine {
            true => self.pass("LATENCY", detail, out),
            false => self.fail("LATENCY", detail, out),
        }
    }
}
