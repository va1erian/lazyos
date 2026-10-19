//! Showing the picture ahead of the page: what the wheel asked for and the page has not done yet.
//!
//! The page answers a wheel notch some tens of milliseconds later (its process lays out, runs the
//! page's scroll handlers, paints), and on a busy page hundreds. Until then the window used to show
//! nothing new. Like the browsers of the first smartphones it now moves the picture it has at once
//! and leaves what that uncovers as a checkerboard until the page paints it: a notch is seen in a
//! frame, the page catches up when it can.
//!
//! The model is only numbers (no xui type, so the rules are tested on the host, `tests/speculate.rs`).
//! `pending` is how far the picture is shown ahead, in page pixels (positive: the page scrolled
//! down, the picture moved up). A wheel notch adds [`LINE_PX`] (the step WebKit takes for one: it
//! reads only the sign of a line event, `Scrollbar::pixelsPerLineStep`); every update the page sends
//! carries the main frame's scroll offset the pixels were painted at (`WKPageSetScrollOffsetCallback`,
//! patch 0028), and a change of it is the page having done that much of what was asked: `pending`
//! gives it back. The picture is exact again when the page has caught up.
//!
//! The wheel does not always scroll the page by what was asked: at the end of the page, over a box
//! that scrolls by itself, with a handler that cancels it. Then the offset never moves and the picture
//! would stay ahead for good. A burst the page has not moved for [`PROBE_MS`] is dropped and the
//! direction is not shown ahead again until the page moves, and whatever is left [`SETTLE_MS`] after
//! the last wheel notch or update is dropped (the picture then jumps back to what the page shows).
//! The offset is only ever compared with its earlier values, so a stale absolute one (a new page that
//! started at the top) costs nothing. The ends of the page are known without waiting: nothing is shown
//! ahead above an offset of zero, nor below the newest pixels when the engine says the page could not
//! scroll further down from where they were painted ([`Speculation::set_at_bottom`]) and the bands
//! below the view are no more than [`END_SLACK`] rows (the page's own margin: the engine paints a few
//! rows past the view's edge even at the end); a page scrolled to its end would otherwise show rows past
//! it, a checkerboard, until [`PROBE_MS`] gave them up. Bands reaching further say the page goes on (the
//! engine's state is not told again when the page grows, only when it scrolls).

/// Pixels a wheel notch moves the page: WebKit's line step.
pub const LINE_PX: i32 = 40;
/// The most the picture is shown ahead, in either direction: past it the checkerboard is most of the view.
pub const MAX_AHEAD: i32 = 480;
/// Rows the bands may reach below the view at the end of the page without the page going on: the engine
/// paints a few rows past the view's edge even at the document's end (29 at webkit.org at 2560x1440).
pub const END_SLACK: i32 = 64;
/// The most the bands let the picture be shown ahead (a view of 1440 rows, three of them).
pub const MAX_REACH: i32 = 4320;
/// Notches one wheel event is trusted for (the engine forwards that many, as before).
pub const MAX_TICKS: i32 = 8;
/// A burst the page has not moved at all for this long is not a scroll the page follows.
pub const PROBE_MS: u64 = 900;
/// What is still ahead this long after the last wheel notch or update is dropped. More than the
/// page's slowest answer seen on a busy page (450 ms): dropped earlier, the picture would jump back
/// and then forward again when the answer came.
pub const SETTLE_MS: u64 = 500;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Wheel events that moved the picture ahead.
    pub shifts: u32,
    /// Times what was ahead was dropped, not given back by the page (it jumped back).
    pub snaps: u32,
    /// The most pixels one such drop undid.
    pub snap_px: i32,
    /// Wheel events not shown ahead because the page did not follow the direction.
    pub blocked: u32,
}

#[derive(Debug, Default)]
pub struct Speculation {
    /// How far ahead may go past [`MAX_AHEAD`], down and up: as far as the bands the page's process
    /// painted outside the view reach (`bands.rs`); zero without them.
    reach_down: i32,
    reach_up: i32,
    /// The newest pixels were painted where the page could not scroll further down.
    at_bottom: bool,
    pending: i32,
    actual: Option<i32>,
    /// The time of the last wheel notch or update.
    last_ms: u64,
    /// When `pending` last left zero.
    burst_ms: u64,
    /// The page's offset changed since `pending` left zero.
    moved: bool,
    /// The sign of a direction the page did not follow: not shown ahead until the page moves.
    blocked: i32,
    counters: Counters,
}

impl Speculation {
    /// How far the picture is shown ahead (page pixels, positive: moved up).
    pub fn shift(&self) -> i32 {
        self.pending
    }

    /// The offset of the pixels in the picture, once an update told it.
    pub fn actual(&self) -> Option<i32> {
        self.actual
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    /// A vertical wheel event of `ticks` notches (negative: down, as xui's wheel and the engine's).
    /// Returns whether the picture moved (ahead, or back from a burst the page never followed).
    pub fn wheel(&mut self, ticks: i32, now_ms: u64) -> bool {
        let dropped = self.drop_stalled(now_ms);
        let step = -ticks.clamp(-MAX_TICKS, MAX_TICKS) * LINE_PX;
        if step == 0 {
            return dropped;
        }
        self.last_ms = now_ms;
        if step.signum() == -self.blocked {
            self.blocked = 0;
        }
        if step.signum() == self.blocked {
            self.counters.blocked += 1;
            return dropped;
        }
        // Never above the top of the page; an offset that is not one (negative) allows nothing upward.
        let up = MAX_AHEAD.max(self.reach_up);
        let floor = self
            .actual
            .map_or(-MAX_AHEAD, |top| top.saturating_neg().clamp(-up, 0));
        // Never below the end of the page: when the engine says it is pinned to the bottom and no more
        // than a margin of rows is painted below the view, nothing further is shown ahead, instead of the
        // usual [`MAX_AHEAD`] of checkerboard past a document that does not go on. What is already ahead
        // stays (a notch down never moves the picture up).
        let ceiling = if self.at_end() {
            self.pending.max(0)
        } else {
            MAX_AHEAD.max(self.reach_down)
        };
        let next = (self.pending + step).clamp(floor, ceiling);
        if next == self.pending {
            return dropped;
        }
        if self.pending == 0 {
            self.burst_ms = now_ms;
            self.moved = false;
        }
        self.pending = next;
        self.counters.shifts += 1;
        true
    }

    /// An update arrived whose pixels were painted at the main frame's scroll offset `offset`.
    /// Returns whether what is shown ahead changed.
    pub fn report(&mut self, offset: i32, now_ms: u64) -> bool {
        let Some(before) = self.actual.replace(offset) else {
            self.last_ms = now_ms;
            return false;
        };
        let moved_by = offset.saturating_sub(before);
        if moved_by == 0 {
            // An update that did not scroll (a video, an animation, a carousel) is not the page
            // answering the wheel: it must not keep what is ahead from being given up on.
            return false;
        }
        self.last_ms = now_ms;
        self.blocked = 0;
        if self.pending == 0 {
            return false;
        }
        self.moved = true;
        let ahead = self.pending;
        // The page went the other way, or further than was asked: nothing to show ahead of it.
        self.pending = if moved_by.signum() == ahead.signum()
            && moved_by.unsigned_abs() < ahead.unsigned_abs()
        {
            ahead - moved_by
        } else {
            0
        };
        if self.pending == 0 {
            self.moved = false;
        }
        true
    }

    /// Time passes with nothing arriving: what is ahead is dropped once the page has had its say.
    /// Returns whether the picture jumped back.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        if self.pending == 0 {
            return false;
        }
        if self.drop_stalled(now_ms) {
            return true;
        }
        if now_ms.saturating_sub(self.last_ms) >= SETTLE_MS {
            self.snap();
            return true;
        }
        false
    }

    /// How far past the view's bottom (`down`) and top (`up`) the page's rows are at hand (the bands);
    /// the picture may be shown that far ahead, and not less than [`MAX_AHEAD`] unless the page ends.
    pub fn set_reach(&mut self, down: i32, up: i32) {
        self.reach_down = down.clamp(0, MAX_REACH);
        self.reach_up = up.clamp(0, MAX_REACH);
    }

    /// The page ends where the newest pixels end: the engine says it could not scroll further down
    /// from their offset and the bands below the view are only its margin ([`END_SLACK`]).
    fn at_end(&self) -> bool {
        self.at_bottom && self.reach_down <= END_SLACK
    }

    /// Whether the page could not scroll further down from the offset of the newest update
    /// (`WKPageIsPinnedToBottomSide`, told with the offset): then no wheel notch shows the picture
    /// ahead downward (unless the bands say the page goes on: the state can be stale). What is ahead
    /// downward is past the end of the page and is dropped at once, not after [`PROBE_MS`]: returns
    /// whether any was (the picture goes back).
    pub fn set_at_bottom(&mut self, at_bottom: bool) -> bool {
        self.at_bottom = at_bottom;
        if self.at_end() && self.pending > 0 {
            self.snap();
            return true;
        }
        false
    }

    /// The picture is not the page's any more (a new size, a crashed process): nothing is ahead.
    pub fn reset(&mut self) {
        self.reach_down = 0;
        self.reach_up = 0;
        self.at_bottom = false;
        self.pending = 0;
        self.actual = None;
        self.blocked = 0;
        self.moved = false;
    }

    /// Whether anything is ahead, and so whether a timer is needed to drop it.
    pub fn is_ahead(&self) -> bool {
        self.pending != 0
    }

    fn drop_stalled(&mut self, now_ms: u64) -> bool {
        if self.pending != 0 && !self.moved && now_ms.saturating_sub(self.burst_ms) >= PROBE_MS {
            self.blocked = self.pending.signum();
            self.snap();
            return true;
        }
        false
    }

    fn snap(&mut self) {
        self.counters.snaps += 1;
        self.counters.snap_px = self.counters.snap_px.max(self.pending.abs());
        self.pending = 0;
        self.moved = false;
    }
}

/// Milliseconds on a clock that only goes forward, from the first call.
pub fn now_ms() -> u64 {
    now_us() / 1000
}

/// Microseconds on the same clock as [`now_ms`].
pub fn now_us() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// What the window reports about it once a second, `INFO:SPEC:`, while it is in use.
///
///   `INFO:SPEC:shifts=<n>:snaps=<n>:snap_px=<most>:blocked=<n>:w2s=<avg us>/<max us>:ahead=<most px>`
///
/// `w2s` is a wheel event to the first draw that shows it (what the page's answer used to be).
#[derive(Debug, Default)]
pub struct Meter {
    since_ms: u64,
    seen: Counters,
    latency_us: u64,
    latencies: u32,
    latency_max: u64,
    ahead_max: i32,
}

impl Meter {
    /// A wheel event was drawn `us` microseconds after it arrived.
    pub fn drawn(&mut self, us: u64) {
        self.latency_us += us;
        self.latencies += 1;
        self.latency_max = self.latency_max.max(us);
    }

    /// The line to print at `now_ms`, if a second has passed and the picture was moved ahead in it.
    pub fn line(&mut self, now_ms: u64, counters: Counters, ahead: i32) -> Option<String> {
        self.ahead_max = self.ahead_max.max(ahead.abs());
        if now_ms.saturating_sub(self.since_ms) < 1000 {
            return None;
        }
        self.since_ms = now_ms;
        let shifts = counters.shifts - self.seen.shifts;
        let snaps = counters.snaps - self.seen.snaps;
        let blocked = counters.blocked - self.seen.blocked;
        let (snap_px, ahead_max) = (counters.snap_px, std::mem::take(&mut self.ahead_max));
        self.seen = counters;
        if shifts == 0 && snaps == 0 && blocked == 0 {
            return None;
        }
        let average = self.latency_us / u64::from(self.latencies.max(1));
        let line = format!(
            "INFO:SPEC:shifts={shifts}:snaps={snaps}:snap_px={snap_px}:blocked={blocked}:w2s={average}/{}:ahead={ahead_max}",
            self.latency_max
        );
        self.latency_us = 0;
        self.latencies = 0;
        self.latency_max = 0;
        Some(line)
    }
}
