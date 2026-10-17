//! The picture shown ahead of the page (`speculate.rs`): the rules, and a seeded simulation of a
//! page that follows the wheel late, in bursts, and not at all past its end.

use xui_webkit::speculate::{Meter, Speculation, LINE_PX, MAX_AHEAD, PROBE_MS, SETTLE_MS};

const DOWN: i32 = -1;
const UP: i32 = 1;

#[test]
fn a_notch_shows_at_once_and_the_page_gives_it_back() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    assert!(s.wheel(DOWN, 10));
    assert_eq!(s.shift(), LINE_PX);
    assert!(s.wheel(2 * DOWN, 20));
    assert_eq!(s.shift(), 3 * LINE_PX);
    // The page did the first two notches.
    assert!(s.report(1000 + 2 * LINE_PX, 90));
    assert_eq!(s.shift(), LINE_PX);
    assert!(s.report(1000 + 3 * LINE_PX, 110));
    assert_eq!(s.shift(), 0);
    assert!(!s.is_ahead());
    assert_eq!(
        s.counters().snaps,
        0,
        "the page caught up: nothing jumped back"
    );
}

#[test]
fn up_is_the_other_way_and_the_top_is_a_floor() {
    let mut s = Speculation::default();
    s.report(2 * LINE_PX, 0);
    assert!(s.wheel(UP, 1));
    assert_eq!(s.shift(), -LINE_PX);
    assert!(s.wheel(UP, 2));
    assert_eq!(s.shift(), -2 * LINE_PX);
    assert!(
        !s.wheel(UP, 3),
        "the page is at its top: nothing more is shown ahead"
    );
    assert_eq!(s.shift(), -2 * LINE_PX);
}

#[test]
fn the_picture_is_never_shown_further_ahead_than_the_limit() {
    let mut s = Speculation::default();
    s.report(100_000, 0);
    for t in 0..200 {
        s.wheel(DOWN * 8, t);
        assert!(s.shift() <= MAX_AHEAD);
    }
    assert_eq!(s.shift(), MAX_AHEAD);
}

#[test]
fn a_page_that_does_not_move_is_dropped_and_the_direction_is_not_tried_again() {
    let mut s = Speculation::default();
    s.report(500, 0);
    // Nothing ever comes back (the end of the page, a box that scrolls by itself).
    s.wheel(DOWN, 10);
    s.wheel(DOWN, 300);
    s.wheel(DOWN, 600);
    assert!(s.shift() > 0);
    s.wheel(DOWN, 10 + PROBE_MS);
    assert_eq!(
        s.shift(),
        0,
        "{PROBE_MS} ms of notches and the page never moved"
    );
    assert_eq!(s.counters().snaps, 1);
    assert!(!s.wheel(DOWN, 10 + PROBE_MS + 10), "not shown ahead again");
    assert_eq!(s.counters().blocked, 2);
    // The page moves (the user scrolled the other way, or a key): the direction is open again.
    s.report(400, 1900);
    assert!(s.wheel(DOWN, 1910));
    assert_eq!(s.shift(), LINE_PX);
}

#[test]
fn what_is_left_when_the_page_and_the_wheel_go_quiet_is_dropped() {
    let mut s = Speculation::default();
    s.report(0, 0);
    s.wheel(DOWN, 0);
    s.wheel(DOWN, 10);
    s.report(LINE_PX, 100);
    assert_eq!(s.shift(), LINE_PX);
    assert!(!s.tick(100 + SETTLE_MS - 1));
    assert!(s.tick(100 + SETTLE_MS));
    assert_eq!(s.shift(), 0);
    assert_eq!(s.counters().snaps, 1);
    assert_eq!(s.counters().snap_px, LINE_PX);
    assert!(!s.tick(10_000), "nothing ahead: nothing to drop");
}

#[test]
fn a_page_that_went_the_other_way_or_further_leaves_nothing_ahead() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    s.wheel(DOWN, 1);
    s.report(1000 - 300, 2);
    assert_eq!(
        s.shift(),
        0,
        "moved up while the picture was ahead going down"
    );
    s.wheel(DOWN, 3);
    s.report(1000 - 300 + 5 * LINE_PX, 4);
    assert_eq!(
        s.shift(),
        0,
        "moved further than was asked (a key, a smooth scroll)"
    );
}

#[test]
fn a_new_pages_offset_back_at_the_top_costs_nothing() {
    let mut s = Speculation::default();
    s.report(9000, 0);
    s.report(0, 5);
    assert_eq!(s.shift(), 0);
    assert!(s.wheel(DOWN, 6));
    assert_eq!(s.shift(), LINE_PX);
    // Before the first update of a page nothing is known and the floor is the limit.
    s.reset();
    assert_eq!(s.shift(), 0);
    assert!(s.wheel(UP, 7));
    assert_eq!(s.shift(), -LINE_PX);
}

#[test]
fn a_wheel_event_of_nothing_is_nothing() {
    let mut s = Speculation::default();
    assert!(!s.wheel(0, 1));
    assert_eq!(s.shift(), 0);
}

#[test]
fn updates_that_do_not_scroll_do_not_keep_the_picture_ahead() {
    // A page that moved 50 px of the 120 asked, then only repaints (a video, an ad, a carousel).
    let mut s = Speculation::default();
    s.report(1000, 0);
    s.wheel(3 * DOWN, 10);
    assert_eq!(s.shift(), 3 * LINE_PX);
    s.report(1050, 60);
    assert_eq!(s.shift(), 70);
    for t in (100..2000).step_by(16) {
        s.report(1050, t);
        s.tick(t);
    }
    assert_eq!(
        s.shift(),
        0,
        "the page said its last word at 60 ms: dropped {SETTLE_MS} ms later"
    );
    assert_eq!(s.counters().snaps, 1);
}

#[test]
fn hostile_offsets_neither_panic_nor_leave_the_picture_adrift() {
    let mut s = Speculation::default();
    s.report(i32::MIN, 0);
    s.wheel(DOWN, 1);
    s.report(i32::MAX, 2);
    assert!(s.shift().abs() <= MAX_AHEAD);
    s.report(i32::MIN, 3);
    assert!(s.shift().abs() <= MAX_AHEAD);
    s.wheel(UP, 4);
    s.report(0, 5);
    s.report(i32::MAX, 6);
    s.report(-1, 7);
    assert!(s.shift().abs() <= MAX_AHEAD);
}
/// A tiny deterministic generator (no dependency for a test).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A page with `limit` pixels of scroll that handles each wheel notch `latency` ms after it was
/// sent, one at a time, and reports its offset after each (as an update would).
struct Page {
    offset: i32,
    limit: i32,
    queue: Vec<(u64, i32)>,
    follows: bool,
    /// The page was asked for more than it had (its end, or its top): no model can know that.
    clamped: bool,
}

impl Page {
    fn send(&mut self, now: u64, ticks: i32, latency: u64) {
        self.queue.push((now + latency, ticks));
    }

    /// The offsets the page reports by `now`.
    fn run(&mut self, now: u64) -> Vec<(u64, i32)> {
        let mut reports = Vec::new();
        while let Some(&(due, ticks)) = self.queue.first() {
            if due > now {
                break;
            }
            self.queue.remove(0);
            if self.follows {
                let wanted = self.offset - ticks.clamp(-8, 8) * LINE_PX;
                let next = wanted.clamp(0, self.limit);
                self.clamped |= next != wanted;
                self.offset = next;
                reports.push((due, self.offset));
            }
        }
        reports
    }
}

#[test]
fn simulated_pages_the_picture_never_strays_and_comes_back_to_zero() {
    for seed in 1..=400u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let limit = [0, 300, 4000, 100_000][rng.below(4) as usize];
        let mut page = Page {
            offset: if limit == 0 {
                0
            } else {
                (rng.below(limit as u64 + 1) as i32 / LINE_PX) * LINE_PX
            },
            limit,
            queue: Vec::new(),
            follows: rng.below(8) != 0,
            clamped: false,
        };
        let mut s = Speculation::default();
        s.report(page.offset, 0);
        let latency = 20 + rng.below(400);
        let mut now = 0u64;
        let wheel_until = 200 + rng.below(3000);
        while now < wheel_until + PROBE_MS + 4 * SETTLE_MS + latency + 2000 {
            now += 1 + rng.below(40);
            if now < wheel_until && rng.below(3) == 0 {
                let ticks = if rng.below(5) == 0 { 1 } else { -1 } * (1 + rng.below(3) as i32);
                page.send(now, ticks, latency);
                s.wheel(ticks, now);
            }
            for (_, offset) in page.run(now) {
                s.report(offset, now);
            }
            s.tick(now);
            assert!(
                s.shift().abs() <= MAX_AHEAD,
                "seed {seed}: {} ahead",
                s.shift()
            );
            if let Some(top) = s.actual() {
                assert!(
                    top + s.shift() >= 0,
                    "seed {seed}: ahead of the top of the page"
                );
            }
        }
        assert_eq!(
            s.shift(),
            0,
            "seed {seed}: still ahead after everything went quiet"
        );
        if page.follows && !page.clamped {
            // A page that answered every notch in full: what was shown ahead was always given back.
            assert_eq!(
                s.counters().snaps,
                0,
                "seed {seed}: the picture jumped back on a page that followed (latency {latency}, wheel until {wheel_until}, {:?}, offset {})",
                s.counters(),
                page.offset
            );
        }
    }
}

#[test]
fn the_meter_reports_a_second_of_use_and_then_starts_again() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    let mut meter = Meter::default();
    s.wheel(DOWN, 10);
    s.wheel(DOWN, 20);
    meter.drawn(900);
    meter.drawn(1500);
    assert_eq!(
        meter.line(500, s.counters(), s.shift()),
        None,
        "not a second yet"
    );
    s.report(1000 + 2 * LINE_PX, 600);
    let line = meter.line(1000, s.counters(), s.shift()).unwrap();
    assert_eq!(
        line,
        "INFO:SPEC:shifts=2:snaps=0:snap_px=0:blocked=0:w2s=1200/1500:ahead=80"
    );
    assert_eq!(
        meter.line(2100, s.counters(), s.shift()),
        None,
        "nothing happened in that second"
    );
    s.wheel(UP, 2200);
    let next = meter.line(3100, s.counters(), s.shift()).unwrap();
    assert!(next.starts_with("INFO:SPEC:shifts=1:"), "{next}");
}
