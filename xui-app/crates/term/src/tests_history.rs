//! Scrollback: what enters, what does not, the budget, the absolute numbering.

use crate::fuzz::check;
use crate::{Cell, Term};

/// The text of absolute line `line` (history lines padded as stored).
fn text(term: &Term, line: u64) -> Option<String> {
    let found = term.line(line)?;
    let text: String = found
        .cells
        .iter()
        .filter(|cell| cell.is_glyph())
        .map(|cell| cell.ch)
        .collect();
    Some(text.trim_end().to_string())
}

/// `n` numbered lines, `\r\n` terminated.
fn lines(from: usize, to: usize) -> String {
    (from..=to).map(|n| format!("L{n}\r\n")).collect()
}

#[test]
fn rows_leaving_the_top_of_the_main_screen_are_kept_in_order() {
    let mut term = Term::new(10, 4);
    term.feed(lines(1, 10).as_bytes());
    check(&term);
    // 10 lines + the cursor row on 4 rows: 7 scrolled off.
    assert_eq!(term.scrollback_len(), 7);
    assert_eq!(term.first_line(), 0);
    assert_eq!(term.screen_top(), 7);
    let kept: Vec<_> = (0..7).map(|n| text(&term, n).unwrap()).collect();
    assert_eq!(kept, ["L1", "L2", "L3", "L4", "L5", "L6", "L7"]);
    assert_eq!(text(&term, 7).unwrap(), "L8", "the screen follows the history");
    assert!(term.line(7 + 4).is_none());
}

#[test]
fn trailing_blanks_are_not_stored_and_the_wrap_flag_is() {
    let mut term = Term::new(10, 2);
    term.feed(b"ab\r\nabcdefghijklm\r\nz\r\n");
    assert_eq!(term.line(0).unwrap().cells.len(), 2);
    assert!(!term.line(0).unwrap().wrapped);
    let wrapped = term.line(1).unwrap();
    assert_eq!(wrapped.cells.len(), 10, "a full row keeps its width");
    assert!(wrapped.wrapped);
    assert_eq!(text(&term, 2).unwrap(), "klm");
}

#[test]
fn a_blank_row_keeps_no_cells_but_an_erased_background_does() {
    let mut term = Term::new(8, 2);
    term.feed(b"\r\n\r\n\x1b[44m\x1b[K\r\nx\r\n");
    assert_eq!(term.line(0).unwrap().cells.len(), 0);
    assert_eq!(term.line(1).unwrap().cells.len(), 0);
    let tinted = term.line(2).unwrap();
    assert_eq!(tinted.cells.len(), 8, "the blue background is content");
    assert_ne!(tinted.cells[0], Cell::default());
}

#[test]
fn the_budget_evicts_the_oldest_and_numbers_are_not_reused() {
    let mut term = Term::new(10, 3);
    term.set_scrollback(5);
    term.feed(lines(1, 20).as_bytes());
    check(&term);
    assert_eq!(term.scrollback_len(), 5);
    assert_eq!(term.screen_top(), 18, "20 lines on 3 rows scrolled 18 off");
    assert_eq!(term.first_line(), 13);
    assert!(term.line(12).is_none(), "evicted");
    assert_eq!(text(&term, 13).unwrap(), "L14");
    assert_eq!(text(&term, 17).unwrap(), "L18");
    // The same absolute line keeps its text while it is kept.
    let before = text(&term, 15).unwrap();
    term.feed(lines(21, 22).as_bytes());
    assert_eq!(text(&term, 15).unwrap(), before);
    assert!(term.line(15).is_some() && term.first_line() == 15);
}

#[test]
fn a_smaller_budget_drops_at_once_and_zero_keeps_nothing_but_still_counts() {
    let mut term = Term::new(10, 3);
    term.feed(lines(1, 12).as_bytes());
    let top = term.screen_top();
    term.set_scrollback(4);
    assert_eq!(term.scrollback_len(), 4);
    assert_eq!(term.screen_top(), top, "the screen keeps its number");
    term.set_scrollback(0);
    assert_eq!(term.scrollback_len(), 0);
    term.feed(lines(13, 15).as_bytes());
    assert_eq!(term.scrollback_len(), 0);
    assert_eq!(term.screen_top(), top + 3, "rows that were not kept still count");
    check(&term);
}

#[test]
fn the_alternate_screen_never_feeds_the_history() {
    let mut term = Term::new(10, 3);
    term.feed(lines(1, 5).as_bytes());
    let kept = term.scrollback_len();
    term.feed(b"\x1b[?1049h");
    term.feed(lines(1, 30).as_bytes());
    assert_eq!(term.scrollback_len(), kept);
    term.feed(b"\x1b[?1049l");
    assert_eq!(term.scrollback_len(), kept);
    check(&term);
}

#[test]
fn only_a_region_starting_at_the_top_feeds_the_history() {
    let mut term = Term::new(10, 5);
    term.feed(b"\x1b[2;4r\x1b[4;1H");
    term.feed(lines(1, 10).as_bytes());
    assert_eq!(term.scrollback_len(), 0, "rows 2-4 scrolled, not the screen");
    let mut term = Term::new(10, 5);
    term.feed(b"\x1b[1;3r\x1b[3;1H");
    term.feed(lines(1, 4).as_bytes());
    assert!(term.scrollback_len() >= 3, "a region from the top does");
}

#[test]
fn deleting_lines_is_not_scrolling() {
    let mut term = Term::new(10, 4);
    term.feed(b"a\r\nb\r\nc\x1b[1;1H\x1b[2M\x1b[2L");
    assert_eq!(term.scrollback_len(), 0);
}

#[test]
fn csi_3_j_and_reset_clear_the_history_but_not_the_numbers() {
    let mut term = Term::new(10, 3);
    term.feed(lines(1, 10).as_bytes());
    let top = term.screen_top();
    assert!(top > 0);
    term.feed(b"\x1b[3J");
    assert_eq!(term.scrollback_len(), 0);
    assert_eq!(term.first_line(), top);
    assert_eq!(term.screen_top(), top);
    assert!(term.line(top - 1).is_none());
    assert!(text(&term, top).is_some(), "the screen is untouched");
    term.feed(lines(11, 20).as_bytes());
    assert!(term.scrollback_len() > 0);
    let before = term.screen_top();
    term.set_scrollback(7);
    term.feed(b"\x1bc");
    assert_eq!(term.scrollback_len(), 0);
    assert_eq!(term.screen_top(), before);
    term.feed(lines(1, 30).as_bytes());
    assert_eq!(term.scrollback_len(), 7, "the budget survives a reset");
    check(&term);
}

#[test]
fn ten_thousand_short_lines_stay_small() {
    let mut term = Term::new(80, 24);
    for n in 0..12_000 {
        term.feed(format!("line {n}\r\n").as_bytes());
    }
    assert_eq!(term.scrollback_len(), crate::DEFAULT_SCROLLBACK);
    let cells: usize = (term.first_line()..term.screen_top())
        .map(|line| term.line(line).unwrap().cells.len())
        .sum();
    assert!(cells * std::mem::size_of::<Cell>() < 3 << 20, "{cells} cells");
}

#[test]
fn a_resize_keeps_the_history() {
    let mut term = Term::new(10, 3);
    term.feed(lines(1, 8).as_bytes());
    let (len, lines) = (term.scrollback_len(), term.scrollback_len() + term.rows());
    term.resize(6, 5);
    // Two more rows on the screen: two lines came back from the scrollback.
    assert_eq!(term.scrollback_len(), len - 2);
    assert_eq!(term.scrollback_len() + term.rows(), lines);
    check(&term);
}
