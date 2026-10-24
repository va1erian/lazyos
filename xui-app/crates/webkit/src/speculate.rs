//! Showing the picture ahead of the page: what the wheel asked for and the page has not done yet.
//!
//! The page answers a wheel notch some tens of milliseconds later (its process lays out, runs the
//! page's scroll handlers, paints), on a busy page hundreds, and while a heavy page loads up to a
//! second (its scripts run that long and nothing can interrupt them). The window does not wait: like
//! the browsers of the first smartphones it moves the picture it has at once and leaves what that
//! uncovers as a checkerboard until the page paints it (the bands painted ahead of a scroll cover most
//! of it): a notch is seen in a frame, the page catches up when it can.
//!
//! The model is only numbers (no xui type, so the rules are tested on the host, `tests/speculate.rs`).
//! `pending` is how far the picture is shown ahead, in page pixels (positive: the page scrolled
//! down, the picture moved up). A wheel notch adds [`LINE_PX`] (WebKit's step for one); every update
//! the page sends carries the main frame's scroll offset the pixels were painted at
//! (`WKPageSetScrollOffsetCallback`, patch 0028), and a change of it is the page having done that
//! much of what was asked: `pending` gives it back. The picture is exact again when the page has
//! caught up.
//!
//! The wheel does not always scroll the page by what was asked: at the end of the page, over a box
//! that scrolls by itself, with a handler that cancels it. The page says so: every turn is numbered,
//! and the engine answers each once the page has handled it, with where the page then is
//! (`WheelDone`, WebKit patch 0035), before the frame that shows it. So the picture stays ahead for
//! as long as the page is busy, and at each answer it is put where the page went plus the turns still
//! unanswered ([`Speculation::answered`]): exactly what the page did, back to where it stayed when it
//! did nothing (and that direction is not shown ahead again until the page moves). It used to guess
//! instead (given up 500 ms after the last notch), and a page busier than that made the picture jump
//! back and then forward again (issue #940). Only an answer that never comes (a page process that
//! ended) is waited for, [`LOST_MS`].
//!
//! The ends of the page are known without waiting: nothing is shown ahead above an offset of zero,
//! nor below the newest pixels when the engine says the page could not scroll further down from where
//! they were painted ([`Speculation::set_at_bottom`]) and the bands below the view are no more than
//! [`END_SLACK`] rows (the page's own margin: the engine paints a few rows past the view's edge even at
//! the end). Bands reaching further say the page goes on (the engine's state is not told again when
//! the page grows, only when it scrolls).

use std::collections::VecDeque;

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
/// What is still ahead this long after the last notch, page move or answer is dropped: the answers
/// were lost (the page's process ended). Far longer than a busy page takes to answer (a second, while a
/// heavy one loads).
pub const LOST_MS: u64 = 5_000;
/// Unanswered turns remembered; past it the oldest are forgotten (a page that never answers).
const MAX_UNANSWERED: usize = 1024;

/// A page move this soon after a plain wheel notch is the page's answer to it: with nothing shown
/// ahead the view glides to it rather than jumping.
pub const ANSWER_MS: u64 = 1_000;

thread_local! {
    /// Whether the views of this thread show the picture ahead of the page.
    static AHEAD: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// With nothing shown ahead: whether a page move of `moved` pixels (positive: down) at `now_ms` is
/// the answer to what the wheel asked (`asked`: when the last notch came and the pixels asked since
/// the page last answered), soon enough after a notch, the same way and no larger. Returns the
/// verdict and what is still asked (an answer is taken off it; anything else leaves it).
pub fn answer(asked: Option<(u64, i32)>, moved: i32, now_ms: u64) -> (bool, Option<(u64, i32)>) {
    let Some((at, wanted)) = asked else {
        return (false, None);
    };
    let fresh = now_ms.saturating_sub(at) <= ANSWER_MS;
    let follows = moved != 0 && moved.signum() == wanted.signum() && moved.abs() <= wanted.abs();
    if fresh && follows {
        (true, Some((at, wanted - moved)))
    } else {
        (false, asked)
    }
}

/// Turns showing the picture ahead on or off for the views of this thread. On by default: the page's
/// answers to the wheel say how long to keep it ahead and where to put it, so it no longer jumps back
/// and forth on a busy page (issue #940, when it guessed). Off, a notch moves the view when the page has
/// scrolled, gliding there.
pub fn set_ahead(on: bool) {
    AHEAD.with(|ahead| ahead.set(on));
}

/// Whether the views of this thread show the picture ahead of the page ([`set_ahead`]).
pub fn ahead() -> bool {
    AHEAD.with(std::cell::Cell::get)
}

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
    /// The time of the last wheel notch, page move or answer.
    last_ms: u64,
    /// The turns the page has not answered, oldest first: their number and the page pixels they asked
    /// for (positive: down). The page does them whatever the window showed.
    unanswered: VecDeque<(u64, i32)>,
    /// Where the last answer said the page was.
    answered_at: Option<i32>,
    /// Turns up to this number belong to a document that is gone: their answers name its offset, not
    /// this page's, and are ignored (`forget_turns_through`).
    forgotten: u64,
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

    /// A vertical wheel turn of `ticks` notches (negative: down, as xui's wheel and the engine's),
    /// numbered `seq` for the page's answer. Returns whether the picture moved ahead.
    pub fn wheel(&mut self, ticks: i32, now_ms: u64, seq: u64) -> bool {
        let step = -ticks.clamp(-MAX_TICKS, MAX_TICKS) * LINE_PX;
        if step == 0 {
            return false;
        }
        self.last_ms = now_ms;
        if self.unanswered.len() == MAX_UNANSWERED {
            self.unanswered.pop_front();
        }
        self.unanswered.push_back((seq, step));
        if step.signum() == -self.blocked {
            self.blocked = 0;
        }
        if step.signum() == self.blocked {
            self.counters.blocked += 1;
            return false;
        }
        // At the end of the page what is already ahead stays (a notch down never moves the picture up).
        let ceiling = self.ceiling(self.pending.max(0));
        let next = self
            .pending
            .saturating_add(step)
            .clamp(self.floor(), ceiling);
        if next == self.pending {
            return false;
        }
        self.pending = next;
        self.counters.shifts += 1;
        true
    }

    /// The page answered every turn up to `seq` and is scrolled to `offset` now (the frame that shows
    /// it may still be on its way). The picture is put where the page went plus the turns still
    /// unanswered; a page that did nothing of what it answered declined that direction. Returns
    /// whether what is shown ahead changed.
    pub fn answered(&mut self, seq: u64, offset: i32, now_ms: u64) -> bool {
        if seq <= self.forgotten {
            return false;
        }
        let mut done = 0i32;
        while self
            .unanswered
            .front()
            .is_some_and(|(turn, _)| *turn <= seq)
        {
            if let Some((_, step)) = self.unanswered.pop_front() {
                done = done.saturating_add(step);
            }
        }
        // Where the page was before these turns: the last answer, else the painted offset. With neither
        // (a new page, no frame yet) nothing says whether it moved, and nothing is concluded.
        let from = self.answered_at.or(self.actual);
        match from {
            Some(from) if from != offset => self.blocked = 0,
            Some(_) if done != 0 => self.blocked = done.signum(),
            _ => {}
        }
        self.answered_at = Some(offset);
        self.last_ms = now_ms;
        let (Some(actual), Some(from)) = (self.actual, from) else {
            return false;
        };
        // A move the answered turns do not explain (further than they asked, or the other way: End, an
        // anchor, a script) is the frame's to show when it comes, in one step: nothing is shown ahead of it
        // as a checkerboard meanwhile.
        // Measured from the last answer, or from the frames when they moved since (a key scroll they show).
        let asked = i64::from(done);
        let unexplained = |moved: i64| {
            moved != 0 && (moved.abs() > asked.abs() || moved.signum() != asked.signum())
        };
        if unexplained(i64::from(offset) - i64::from(from))
            && unexplained(i64::from(offset) - i64::from(actual))
        {
            let changed = self.pending != 0;
            if changed {
                self.snap();
            }
            return changed;
        }
        let mut ahead = self
            .unanswered
            .iter()
            .fold(0i32, |sum, (_, step)| sum.saturating_add(*step));
        if ahead.signum() == self.blocked {
            ahead = 0;
        }
        let went = i64::from(offset) - i64::from(actual);
        let ceiling = self.ceiling(went.clamp(0, i64::from(MAX_REACH)) as i32);
        let next =
            (went + i64::from(ahead)).clamp(i64::from(self.floor()), i64::from(ceiling)) as i32;
        if next == self.pending {
            return false;
        }
        if self.blocked != 0 && next.abs() < self.pending.abs() {
            self.counters.snaps += 1;
            self.counters.snap_px = self.counters.snap_px.max(self.pending.abs() - next.abs());
        }
        self.pending = next;
        true
    }

    /// The most the picture may be shown ahead upward: never above the top of the page (an offset that
    /// is not one, negative, allows nothing upward).
    fn floor(&self) -> i32 {
        let up = MAX_AHEAD.max(self.reach_up);
        self.actual
            .map_or(-MAX_AHEAD, |top| top.saturating_neg().clamp(-up, 0))
    }

    /// The most it may be shown ahead downward: as far as the bands reach, and at the known end of the
    /// page no further than `at_end` (what is already ahead, or what the page itself did).
    fn ceiling(&self, at_end: i32) -> i32 {
        if self.at_end() {
            at_end
        } else {
            MAX_AHEAD.max(self.reach_down)
        }
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
            // An update that did not scroll (a video, an animation, a carousel) says nothing new.
            return false;
        }
        self.last_ms = now_ms;
        self.blocked = 0;
        if self.pending == 0 {
            return false;
        }
        let ahead = self.pending;
        // The page went the other way, or further than was asked: nothing to show ahead of it (the
        // next answer says where it is going).
        self.pending = if moved_by.signum() == ahead.signum()
            && moved_by.unsigned_abs() < ahead.unsigned_abs()
        {
            ahead - moved_by
        } else {
            0
        };
        true
    }

    /// Time passes with nothing arriving: what is ahead is dropped when the answers were lost.
    /// Returns whether the picture jumped back.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        if now_ms.saturating_sub(self.last_ms) < LOST_MS {
            return false;
        }
        self.unanswered.clear();
        if self.pending == 0 {
            return false;
        }
        self.snap();
        true
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
    /// downward is past the end of the page and is dropped at once, before the page answers: returns
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
        self.unanswered.clear();
        self.answered_at = None;
        self.blocked = 0;
    }

    /// Another document took the view (a navigation): the turns up to `seq` were the old one's, and an
    /// answer to them, still on its way, names where the old page was. Applied to the new page it would
    /// show it far ahead of what it painted, a checkerboard, until the next scroll.
    pub fn forget_turns_through(&mut self, seq: u64) {
        self.forgotten = self.forgotten.max(seq);
    }

    /// Whether anything is ahead, and so whether a timer is needed to drop it.
    pub fn is_ahead(&self) -> bool {
        self.pending != 0
    }

    fn snap(&mut self) {
        self.counters.snaps += 1;
        self.counters.snap_px = self.counters.snap_px.max(self.pending.abs());
        self.pending = 0;
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
