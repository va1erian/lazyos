//! Selection: the shapes, the text they give, and when they are dropped.

use crate::{Point, SelectMode, Term};

fn p(line: u64, col: usize) -> Point {
    Point::new(line, col)
}

/// A 20x5 screen with three rows of text.
fn text_screen() -> Term {
    let mut term = Term::new(20, 5);
    term.feed(b"hello world\r\nsecond line\r\nthird");
    term
}

fn drag(term: &mut Term, mode: SelectMode, from: Point, to: Point) -> Option<String> {
    term.select_begin(mode, from);
    term.select_update(to);
    term.selection_text()
}

#[test]
fn a_character_drag_spans_rows_with_a_newline() {
    let mut term = text_screen();
    assert_eq!(drag(&mut term, SelectMode::Char, p(0, 6), p(1, 5)).unwrap(), "world\nsecond");
    // Dragging backwards gives the same text.
    assert_eq!(drag(&mut term, SelectMode::Char, p(1, 5), p(0, 6)).unwrap(), "world\nsecond");
}

#[test]
fn a_click_without_a_drag_selects_nothing() {
    let mut term = text_screen();
    term.select_begin(SelectMode::Char, p(0, 3));
    assert!(!term.has_selection());
    assert!(term.selection_text().is_none());
    assert!(term.selection_span(0).is_none());
}

#[test]
fn trailing_blanks_are_trimmed_and_hard_newlines_kept() {
    let mut term = Term::new(10, 4);
    term.feed(b"ab   \r\n\r\ncd");
    assert_eq!(drag(&mut term, SelectMode::Char, p(0, 0), p(2, 9)).unwrap(), "ab\n\ncd");
}

#[test]
fn wrapped_rows_join_without_a_newline() {
    let mut term = Term::new(10, 4);
    term.feed(b"abcdefghijklmno\r\nxyz");
    assert_eq!(drag(&mut term, SelectMode::Char, p(0, 0), p(1, 4)).unwrap(), "abcdefghijklmno");
    assert_eq!(drag(&mut term, SelectMode::Char, p(0, 7), p(2, 1)).unwrap(), "hijklmno\nxy");
}

#[test]
fn a_double_click_selects_a_word_with_the_extra_characters() {
    let mut term = Term::new(30, 3);
    term.feed(b"cd /usr/local-bin/x.y now");
    term.select_begin(SelectMode::Word, p(0, 8));
    assert_eq!(term.selection_text().unwrap(), "/usr/local-bin/x.y");
    term.select_begin(SelectMode::Word, p(0, 1));
    assert_eq!(term.selection_text().unwrap(), "cd");
    term.set_word_chars("");
    term.select_begin(SelectMode::Word, p(0, 5));
    assert_eq!(term.selection_text().unwrap(), "usr");
}

#[test]
fn a_word_selection_widens_by_whole_words_when_dragged() {
    let mut term = text_screen();
    term.select_begin(SelectMode::Word, p(0, 1));
    term.select_update(p(0, 8));
    assert_eq!(term.selection_text().unwrap(), "hello world");
    term.select_update(p(1, 2));
    assert_eq!(term.selection_text().unwrap(), "hello world\nsecond");
}

#[test]
fn blanks_and_punctuation_are_words_of_their_own() {
    let mut term = Term::new(20, 2);
    term.feed(b"a   (b)");
    term.select_begin(SelectMode::Word, p(0, 2));
    assert_eq!(term.selection_span(0), Some((1, 3)));
    term.select_begin(SelectMode::Word, p(0, 4));
    assert_eq!(term.selection_text().unwrap(), "(");
}

#[test]
fn a_triple_click_selects_the_whole_logical_line() {
    let mut term = Term::new(10, 4);
    term.feed(b"abcdefghijklmno\r\nxyz");
    term.select_begin(SelectMode::Line, p(1, 2));
    assert_eq!(term.selection_text().unwrap(), "abcdefghijklmno", "from the second row");
    term.select_begin(SelectMode::Line, p(2, 0));
    assert_eq!(term.selection_text().unwrap(), "xyz");
    term.select_update(p(0, 0));
    assert_eq!(term.selection_text().unwrap(), "abcdefghijklmno\nxyz", "dragging adds whole lines");
}

#[test]
fn a_block_is_a_rectangle() {
    let mut term = text_screen();
    assert_eq!(drag(&mut term, SelectMode::Block, p(0, 2), p(2, 5)).unwrap(), "llo\ncond\nird");
    assert_eq!(term.selection_span(1), Some((2, 5)));
    assert_eq!(drag(&mut term, SelectMode::Block, p(0, 5), p(1, 2)).unwrap(), "llo\ncond");
}

#[test]
fn shift_click_moves_the_nearer_end() {
    let mut term = text_screen();
    drag(&mut term, SelectMode::Char, p(0, 2), p(0, 8));
    term.select_extend(p(1, 3));
    assert_eq!(term.selection_text().unwrap(), "llo world\nseco");
    term.select_extend(p(0, 0));
    assert_eq!(term.selection_text().unwrap(), "hello world\nseco");
}

#[test]
fn shift_click_without_a_selection_extends_from_the_cursor() {
    let mut term = Term::new(20, 3);
    term.feed(b"abc def");
    term.select_extend(p(0, 1));
    assert_eq!(term.selection_text().unwrap(), "bc def");
}

#[test]
fn wide_characters_are_selected_whole_and_their_spacers_dropped() {
    let mut term = Term::new(10, 2);
    term.feed("a\u{4e2d}b".as_bytes());
    // Starting on the spacer (column 2) takes the character with it.
    assert_eq!(drag(&mut term, SelectMode::Char, p(0, 2), p(0, 3)).unwrap(), "\u{4e2d}b");
    // Ending on the head takes its spacer.
    assert_eq!(drag(&mut term, SelectMode::Char, p(0, 0), p(0, 1)).unwrap(), "a\u{4e2d}");
}

#[test]
fn select_all_covers_the_scrollback_and_drops_the_blank_tail() {
    let mut term = Term::new(10, 3);
    term.feed(b"one\r\ntwo\r\nthree\r\nfour");
    term.select_all();
    assert_eq!(term.selection_text().unwrap(), "one\ntwo\nthree\nfour");
    let mut term = text_screen();
    term.select_all();
    assert_eq!(term.selection_text().unwrap(), "hello world\nsecond line\nthird");
}

#[test]
fn the_selection_survives_scrolling_and_is_dropped_when_evicted() {
    let mut term = Term::new(10, 3);
    term.feed(b"keep\r\nx\r\n");
    drag(&mut term, SelectMode::Char, p(0, 0), p(0, 3));
    term.feed(b"1\r\n2\r\n3\r\n4\r\n");
    assert_eq!(term.selection_text().unwrap(), "keep", "the line scrolled into the history");
    assert!(term.line(0).is_some());
    term.set_scrollback(2);
    assert!(!term.has_selection(), "its text left the scrollback");
}

#[test]
fn overwriting_the_selected_text_clears_it_but_other_rows_do_not() {
    let mut term = text_screen();
    drag(&mut term, SelectMode::Char, p(0, 0), p(0, 4));
    term.feed(b"\x1b[3;1Hwritten elsewhere");
    assert!(term.has_selection());
    term.feed(b"\x1b[1;3Hx");
    assert!(!term.has_selection());
    drag(&mut term, SelectMode::Char, p(1, 0), p(1, 4));
    term.feed(b"\x1b[2;1H\x1b[K");
    assert!(!term.has_selection(), "an erase counts as an overwrite");
    drag(&mut term, SelectMode::Char, p(1, 0), p(1, 4));
    term.feed(b"\x1b[1;1H\x1b[2L");
    assert!(!term.has_selection(), "inserting lines moved the text");
}

#[test]
fn the_alternate_screen_and_a_resize_drop_it() {
    let mut term = text_screen();
    drag(&mut term, SelectMode::Char, p(0, 0), p(0, 4));
    term.feed(b"\x1b[?1049h");
    assert!(!term.has_selection());
    term.feed(b"\x1b[?1049l");
    drag(&mut term, SelectMode::Char, p(0, 0), p(0, 4));
    term.resize(10, 5);
    assert!(!term.has_selection());
}

#[test]
fn points_are_clamped_to_what_exists() {
    let mut term = text_screen();
    term.select_begin(SelectMode::Char, p(u64::MAX, 9999));
    term.select_update(p(0, 0));
    assert!(term.selection_text().is_some());
    assert!(term.selection_range().is_some());
}
