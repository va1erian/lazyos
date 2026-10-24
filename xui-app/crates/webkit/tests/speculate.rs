//! The picture shown ahead of the page (`speculate.rs`): the rules (`speculate_sim.rs` simulates pages).

use xui_webkit::speculate::{Meter, Speculation, LINE_PX, LOST_MS, MAX_AHEAD};

const DOWN: i32 = -1;
const UP: i32 = 1;

#[test]
fn a_notch_shows_at_once_and_the_page_gives_it_back() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    assert!(s.wheel(DOWN, 10, 10));
    assert_eq!(s.shift(), LINE_PX);
    assert!(s.wheel(2 * DOWN, 20, 20));
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
    assert!(s.wheel(UP, 1, 1));
    assert_eq!(s.shift(), -LINE_PX);
    assert!(s.wheel(UP, 2, 2));
    assert_eq!(s.shift(), -2 * LINE_PX);
    assert!(
        !s.wheel(UP, 3, 3),
        "the page is at its top: nothing more is shown ahead"
    );
    assert_eq!(s.shift(), -2 * LINE_PX);
}

#[test]
fn the_end_of_the_page_is_a_ceiling_once_the_engine_says_so() {
    let mut s = Speculation::default();
    s.report(5000, 0);
    s.set_at_bottom(true);
    assert!(
        !s.wheel(DOWN, 1, 1),
        "the page cannot scroll further down: nothing is shown past its end"
    );
    assert_eq!(s.shift(), 0);
    assert!(s.wheel(UP, 2, 2), "up still goes ahead");
    assert_eq!(s.shift(), -LINE_PX);
    // Down again from there goes back as far as the newest pixels, no further.
    assert!(s.wheel(4 * DOWN, 3, 3));
    assert_eq!(s.shift(), 0);
    // The page grew (a lazy load): the engine says it is not at the end any more.
    s.set_at_bottom(false);
    assert!(s.wheel(DOWN, 4, 4));
    assert_eq!(s.shift(), LINE_PX);
    s.reset();
    s.report(5000, 10);
    assert!(
        s.wheel(DOWN, 11, 11),
        "a reset forgets the end of the old picture"
    );
}

#[test]
fn rows_painted_below_the_view_outweigh_a_stale_end_of_the_page() {
    let mut s = Speculation::default();
    s.report(0, 0);
    // An overlay kept the page from scrolling, and WebKit has not said otherwise since it closed;
    // but the page's process has painted rows below the view.
    s.set_at_bottom(true);
    s.set_reach(1200, 0);
    assert!(
        s.wheel(DOWN, 1, 1),
        "there are rows below: the notch is shown at once"
    );
    assert_eq!(s.shift(), LINE_PX);
    s.set_reach(0, 0);
    assert!(
        !s.wheel(DOWN, 2, 2),
        "no rows below and the engine says the end: nothing further"
    );
}

#[test]
fn at_the_end_a_margin_of_bands_below_the_view_is_not_a_page_that_goes_on() {
    // webkit.org at its end: the engine says pinned to the bottom, and the bands still reach 29 rows
    // below the view (the engine paints a few rows past the view's edge at the document's end). Notches
    // against the end used to take the picture 480 rows ahead, 450 of them checkerboard.
    let mut s = Speculation::default();
    s.report(665, 0);
    s.set_reach(29, 665);
    assert!(!s.set_at_bottom(true), "nothing was ahead");
    for t in 1..30 {
        assert!(
            !s.wheel(3 * DOWN, t, t),
            "notch {t} is not shown past the end"
        );
    }
    assert_eq!(s.shift(), 0);
    // A page that goes on (bands a view long below it) and a stale pinned state: shown ahead as usual.
    let mut s = Speculation::default();
    s.report(600, 0);
    s.set_reach(1200, 600);
    s.set_at_bottom(true);
    assert!(s.wheel(3 * DOWN, 1, 1));
    assert_eq!(s.shift(), 3 * LINE_PX);
    // Not at the end: the usual allowance, and what is past the margin once the page says it ended
    // is dropped at once.
    let mut s = Speculation::default();
    s.report(600, 0);
    s.set_reach(29, 600);
    for t in 1..10 {
        s.wheel(3 * DOWN, t, t);
    }
    assert_eq!(s.shift(), MAX_AHEAD, "not at the end: the usual allowance");
    assert!(
        s.set_at_bottom(true),
        "what is ahead past the end is dropped"
    );
    assert_eq!(s.shift(), 0);
    assert!(!s.set_at_bottom(true));
}

#[test]
fn what_was_ahead_past_the_end_is_dropped_when_the_page_says_it_ended() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    assert!(s.wheel(4 * DOWN, 1, 1));
    assert_eq!(s.shift(), 4 * LINE_PX);
    // The page went one notch and stopped at its end: the rest is past it, gone at once.
    s.report(1000 + LINE_PX, 40);
    assert!(s.set_at_bottom(true));
    assert_eq!(s.shift(), 0);
    assert_eq!(s.counters().snaps, 1);
    assert!(!s.set_at_bottom(true), "nothing left to drop");
    // Ahead upward is not past the end.
    assert!(s.wheel(UP, 50, 50));
    assert!(!s.set_at_bottom(true));
    assert_eq!(s.shift(), -LINE_PX);
}

#[test]
fn the_picture_is_never_shown_further_ahead_than_the_limit() {
    let mut s = Speculation::default();
    s.report(100_000, 0);
    for t in 0..200 {
        s.wheel(DOWN * 8, t, t);
        assert!(s.shift() <= MAX_AHEAD);
    }
    assert_eq!(s.shift(), MAX_AHEAD);
}

#[test]
fn a_page_that_answers_without_moving_declined_and_the_direction_is_not_tried_again() {
    let mut s = Speculation::default();
    s.report(500, 0);
    // The end of the page, a box that scrolls by itself, a handler that cancels the wheel.
    s.wheel(DOWN, 10, 1);
    s.wheel(DOWN, 300, 2);
    assert_eq!(s.shift(), 2 * LINE_PX);
    assert!(
        s.answered(1, 500, 2000),
        "the page answered the first turn and stayed"
    );
    assert_eq!(
        s.shift(),
        0,
        "what it did not do is given back, and the turn after it too: that way is closed"
    );
    assert_eq!(s.counters().snaps, 1);
    assert!(!s.wheel(DOWN, 2010, 3), "not shown ahead again");
    assert_eq!(s.counters().blocked, 1);
    s.answered(3, 500, 2100);
    assert_eq!(s.shift(), 0);
    // The page moves (the other way, or a key): the direction is open again.
    s.report(400, 2200);
    assert!(s.wheel(DOWN, 2210, 4));
    assert_eq!(s.shift(), LINE_PX);
}

#[test]
fn a_busy_page_keeps_the_picture_ahead_until_it_answers_however_late() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    s.wheel(3 * DOWN, 10, 1);
    s.wheel(2 * DOWN, 20, 2);
    assert_eq!(s.shift(), 5 * LINE_PX);
    // Its script runs for three seconds: nothing comes, nothing is given up.
    for t in (100..3000).step_by(100) {
        assert!(!s.tick(t));
    }
    assert_eq!(s.shift(), 5 * LINE_PX);
    // It answers the first turn (its frame is still on the way): still ahead by the rest.
    assert!(
        !s.answered(1, 1000 + 3 * LINE_PX, 3010),
        "where it went is what was shown"
    );
    assert_eq!(s.shift(), 5 * LINE_PX);
    s.report(1000 + 3 * LINE_PX, 3020);
    assert_eq!(s.shift(), 2 * LINE_PX);
    s.answered(2, 1000 + 5 * LINE_PX, 3030);
    s.report(1000 + 5 * LINE_PX, 3040);
    assert_eq!(s.shift(), 0);
    assert_eq!(s.counters().snaps, 0, "nothing jumped back");
}

#[test]
fn a_page_that_did_part_of_a_turn_is_shown_where_it_stopped() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    s.wheel(3 * DOWN, 10, 1);
    // The last 20 pixels of the page: it went 20 of the 120 asked.
    s.answered(1, 1020, 50);
    assert_eq!(s.shift(), 20);
    s.report(1020, 60);
    assert_eq!(s.shift(), 0);
}

#[test]
fn before_anything_is_known_an_answer_closes_no_direction() {
    // A new page: no frame yet, no answer yet.
    let mut s = Speculation::default();
    s.wheel(DOWN, 1, 1);
    s.answered(1, 0, 2);
    s.report(0, 3);
    assert!(s.wheel(DOWN, 4, 2), "the way down is still open");
    assert_eq!(s.counters().blocked, 0);
    // From here the answer has something to be compared with: staying put is declining.
    s.answered(2, 0, 5);
    assert!(!s.wheel(DOWN, 6, 3));
}

#[test]
fn an_answer_from_a_document_that_is_gone_is_ignored() {
    let mut s = Speculation::default();
    s.report(3000, 0);
    s.wheel(DOWN, 1, 1);
    s.wheel(DOWN, 2, 2);
    // A link: the new page is at its top; the old page's answers are still on their way.
    s.reset();
    s.forget_turns_through(2);
    s.report(0, 3);
    assert!(
        !s.answered(2, 3080, 4),
        "the old page's offset is not this page's"
    );
    assert_eq!(s.shift(), 0);
    // The new page's own turns are answered as usual.
    assert!(s.wheel(DOWN, 5, 3));
    s.answered(3, LINE_PX, 6);
    assert_eq!(s.shift(), LINE_PX);
}

#[test]
fn an_answer_older_than_the_frame_on_show_moves_nothing() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    s.wheel(DOWN, 1, 1);
    s.wheel(DOWN, 2, 2);
    // The frame that shows both turns came before the answer to the first.
    s.report(1000 + 2 * LINE_PX, 3);
    assert_eq!(s.shift(), 0);
    s.answered(1, 1000 + LINE_PX, 4);
    assert_eq!(
        s.shift(),
        0,
        "turn 2, still unanswered, is what the frame already shows"
    );
    assert_eq!(s.counters().snaps, 0);
}

#[test]
fn a_move_the_wheel_did_not_ask_for_is_left_to_its_frame() {
    // End pressed while a turn was on its way: the answer names the end of the page.
    let mut s = Speculation::default();
    s.report(1000, 0);
    s.wheel(DOWN, 1, 1);
    assert_eq!(s.shift(), LINE_PX);
    assert!(s.answered(1, 20_000, 2));
    assert_eq!(
        s.shift(),
        0,
        "no checkerboard ahead of the frame that shows the end"
    );
    s.report(20_000, 3);
    assert_eq!(s.shift(), 0);
    // The next turn is measured from there.
    s.wheel(UP, 4, 2);
    s.answered(2, 20_000 - LINE_PX, 5);
    assert_eq!(s.shift(), -LINE_PX);
}

#[test]
fn after_a_key_scroll_the_frames_show_the_next_turn_is_not_given_back() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    s.wheel(DOWN, 1, 1);
    s.answered(1, 1000 + LINE_PX, 2);
    s.report(1000 + LINE_PX, 3);
    // End: the frames go to the end of the page, no turn answered that.
    s.report(20_000, 4);
    assert!(s.wheel(UP, 5, 2));
    s.answered(2, 20_000 - LINE_PX, 6);
    assert_eq!(s.shift(), -LINE_PX, "still shown where the page went");
    assert_eq!(s.counters().snaps, 0, "nothing jumped back");
}

#[test]
fn answers_that_never_come_are_given_up_on() {
    let mut s = Speculation::default();
    s.report(0, 0);
    s.wheel(DOWN, 0, 1);
    s.wheel(DOWN, 10, 2);
    s.report(LINE_PX, 100);
    assert_eq!(s.shift(), LINE_PX);
    assert!(!s.tick(100 + LOST_MS - 1));
    assert!(
        s.tick(100 + LOST_MS),
        "the page's process ended: nothing will answer"
    );
    assert_eq!(s.shift(), 0);
    assert_eq!(s.counters().snaps, 1);
    assert_eq!(s.counters().snap_px, LINE_PX);
    assert!(!s.tick(100_000), "nothing ahead: nothing to drop");
    // A late answer for the forgotten turns does not bring them back.
    s.answered(2, LINE_PX, 100_001);
    assert_eq!(s.shift(), 0);
}

#[test]
fn a_page_that_went_the_other_way_or_further_leaves_nothing_ahead() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    s.wheel(DOWN, 1, 1);
    s.report(1000 - 300, 2);
    assert_eq!(
        s.shift(),
        0,
        "moved up while the picture was ahead going down"
    );
    s.wheel(DOWN, 3, 3);
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
    assert!(s.wheel(DOWN, 6, 6));
    assert_eq!(s.shift(), LINE_PX);
    // Before the first update of a page nothing is known and the floor is the limit.
    s.reset();
    assert_eq!(s.shift(), 0);
    assert!(s.wheel(UP, 7, 7));
    assert_eq!(s.shift(), -LINE_PX);
}

#[test]
fn a_wheel_event_of_nothing_is_nothing() {
    let mut s = Speculation::default();
    assert!(!s.wheel(0, 1, 1));
    assert_eq!(s.shift(), 0);
}

#[test]
fn updates_that_do_not_scroll_do_not_change_what_is_ahead() {
    // A page that moved 50 px of the 120 asked, then only repaints (a video, an ad, a carousel), and
    // answers: it went no further.
    let mut s = Speculation::default();
    s.report(1000, 0);
    s.wheel(3 * DOWN, 10, 1);
    assert_eq!(s.shift(), 3 * LINE_PX);
    s.report(1050, 60);
    assert_eq!(s.shift(), 70);
    for t in (100..2000).step_by(16) {
        s.report(1050, t);
        s.tick(t);
    }
    assert_eq!(s.shift(), 70, "repaints are not answers");
    s.answered(1, 1050, 2000);
    assert_eq!(s.shift(), 0, "the answer: it stopped at 1050");
}

#[test]
fn hostile_offsets_neither_panic_nor_leave_the_picture_adrift() {
    let mut s = Speculation::default();
    s.report(i32::MIN, 0);
    s.wheel(DOWN, 1, 1);
    s.report(i32::MAX, 2);
    assert!(s.shift().abs() <= MAX_AHEAD);
    s.report(i32::MIN, 3);
    assert!(s.shift().abs() <= MAX_AHEAD);
    s.wheel(UP, 4, 4);
    s.report(0, 5);
    s.report(i32::MAX, 6);
    s.report(-1, 7);
    assert!(s.shift().abs() <= MAX_AHEAD);
    // Answers from webd carry offsets as hostile as frames'.
    for (seq, offset) in [(4, i32::MAX), (5, i32::MIN), (u64::MAX, -1), (0, i32::MAX)] {
        s.wheel(DOWN, 8, 6);
        s.answered(seq, offset, 9);
        assert!(s.shift().abs() <= MAX_AHEAD.max(4320));
        // Never shown above the top of a page whose offset is one (a negative one allows nothing upward).
        if let Some(top) = s.actual().filter(|top| *top >= 0) {
            assert!(i64::from(top) + i64::from(s.shift()) >= 0);
        }
    }
}

#[test]
fn the_meter_reports_a_second_of_use_and_then_starts_again() {
    let mut s = Speculation::default();
    s.report(1000, 0);
    let mut meter = Meter::default();
    s.wheel(DOWN, 10, 10);
    s.wheel(DOWN, 20, 20);
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
    s.wheel(UP, 2200, 2200);
    let next = meter.line(3100, s.counters(), s.shift()).unwrap();
    assert!(next.starts_with("INFO:SPEC:shifts=1:"), "{next}");
}

#[test]
fn the_pages_answer_to_the_wheel_is_bounded_by_what_it_asked() {
    use xui_webkit::speculate::{answer, ANSWER_MS};
    // Two notches down asked 80 px at 1000 ms.
    let asked = Some((1000, 80));
    assert_eq!(
        answer(asked, 40, 1100),
        (true, Some((1000, 40))),
        "half of it"
    );
    assert_eq!(
        answer(Some((1000, 40)), 40, 1150),
        (true, Some((1000, 0))),
        "the rest"
    );
    assert_eq!(
        answer(asked, 81, 1100),
        (false, asked),
        "further than asked: a jump"
    );
    assert_eq!(
        answer(asked, -40, 1100),
        (false, asked),
        "the other way: a jump"
    );
    assert_eq!(answer(asked, 0, 1100), (false, asked), "no move");
    assert_eq!(
        answer(asked, 40, 1000 + ANSWER_MS + 1),
        (false, asked),
        "too late: not the answer"
    );
    assert_eq!(
        answer(None, 40, 1100),
        (false, None),
        "nothing asked (a click, a key)"
    );
    // Up is negative.
    assert_eq!(answer(Some((5, -120)), -120, 10), (true, Some((5, 0))));
}
