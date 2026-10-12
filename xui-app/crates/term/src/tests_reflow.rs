//! Reflow: the main screen and the scrollback rewrap to a new width.

use crate::fuzz::check;
use crate::Term;

/// The logical lines of the whole stream (scrollback, then screen): rows
/// joined by `wrapped`, the last one right-trimmed, trailing empty lines left
/// out.
fn stream(term: &Term) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in term.first_line()..term.screen_top() + term.rows() as u64 {
        let row = term.line(line).unwrap();
        current.extend(row.cells.iter().filter(|cell| cell.is_glyph()).map(|cell| cell.ch));
        if !row.wrapped {
            out.push(current.trim_end().to_string());
            current.clear();
        } else if row.cells.len() < term.cols() {
            // A stored row is short; the rest of a wrapped one is blank.
            current.extend(std::iter::repeat_n(' ', term.cols() - row.cells.len()));
        }
    }
    while out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    out
}

fn sample(cols: usize, rows: usize) -> Term {
    let mut term = Term::new(cols, rows);
    for n in 0..40 {
        let wide = if n % 3 == 0 { "\u{4e2d}\u{6587}" } else { "" };
        term.feed(format!("line {n} {wide}{}\r\n", "x".repeat(n % 17 + 3)).as_bytes());
    }
    term.feed(b"$ ");
    term
}

#[test]
fn narrow_then_widen_gives_back_the_original_lines() {
    for (narrow, rows) in [(7, 5), (10, 12), (13, 3), (2, 8)] {
        let mut term = sample(40, 10);
        let before = stream(&term);
        let screen: Vec<String> = (0..10).map(|r| term.row_text(r)).collect();
        term.resize(narrow, rows);
        check(&term);
        assert_eq!(stream(&term), before, "narrowed to {narrow}");
        term.resize(40, 10);
        check(&term);
        assert_eq!(stream(&term), before, "widened again from {narrow}");
        // The screen is the same text again when nothing was lost above it.
        let after: Vec<String> = (0..10).map(|r| term.row_text(r)).collect();
        assert_eq!(after, screen, "the screen after {narrow}");
    }
}

#[test]
fn the_cursor_stays_on_its_character() {
    let mut term = Term::new(30, 6);
    term.feed(b"first line\r\nhello wide world of text here");
    // Put the cursor on the "w" of "wide".
    term.feed(b"\x1b[2;7H");
    for (cols, rows) in [(8, 6), (5, 9), (11, 4), (40, 6), (3, 3)] {
        term.resize(cols, rows);
        check(&term);
        let (row, col) = term.cursor();
        assert_eq!(term.row(row).cells[col].ch, 'w', "at {cols}x{rows}");
    }
}

#[test]
fn the_cursor_after_the_text_stays_after_it() {
    let mut term = Term::new(10, 4);
    // Exactly one row: the cursor waits at the last column to wrap.
    term.feed(b"0123456789");
    term.resize(20, 4);
    check(&term);
    assert_eq!(term.cursor(), (0, 10));
    term.feed(b"ab");
    assert_eq!(term.row_text(0), "0123456789ab");
    // And narrower: the cursor is on a row of its own after a full one.
    term.resize(4, 6);
    check(&term);
    let (row, col) = term.cursor();
    assert_eq!((term.row_text(row), col), (String::new(), 0));
    assert_eq!(term.row_text(row - 1), "89ab");
    assert!(term.row(row - 1).wrapped);
}

#[test]
fn a_wide_character_is_never_split_across_rows() {
    let line = "a\u{4e2d}b\u{6587}\u{5b57}c\u{4e2d}".repeat(3);
    for cols in 2..16 {
        let mut term = Term::new(30, 8);
        term.feed(line.as_bytes());
        term.feed(b"\r\n");
        term.resize(cols, 8);
        check(&term);
        assert_eq!(stream(&term)[0], line, "at {cols} columns");
    }
    // One column cannot hold a wide character: it becomes a blank.
    let mut term = Term::new(8, 4);
    term.feed("\u{4e2d}x".as_bytes());
    term.resize(1, 4);
    check(&term);
}

#[test]
fn the_scrollback_budget_still_applies_after_a_reflow() {
    let mut term = Term::new(20, 5);
    term.set_scrollback(30);
    for n in 0..100 {
        term.feed(format!("line {n:03} {}\r\n", "y".repeat(10)).as_bytes());
    }
    let first = term.first_line();
    term.resize(8, 5);
    check(&term);
    assert_eq!(term.scrollback_len(), 30, "twice the rows, same budget");
    assert!(term.first_line() > first, "older lines were evicted, their numbers are not reused");
    assert!(stream(&term).last().unwrap().starts_with("line 099"));
}

#[test]
fn the_alternate_screen_is_cropped_and_the_main_one_reflowed() {
    let mut term = Term::new(20, 4);
    term.feed(b"main screen text goes here\r\n$ ");
    term.feed(b"\x1b[?1049h\x1b[Halternate screen line 1\x1b[2;1Hsecond");
    term.resize(10, 3);
    check(&term);
    assert_eq!(term.row_text(0), "alternate", "cropped, not rewrapped");
    term.feed(b"\x1b[?1049l");
    check(&term);
    assert_eq!(stream(&term)[0], "main screen text goes here");
    assert_eq!(term.row_text(term.cursor().0), "$");
}

#[test]
fn a_selection_is_cleared_by_a_resize() {
    let mut term = sample(20, 6);
    let line = term.screen_top();
    term.select_begin(crate::SelectMode::Char, crate::Point::new(line, 0));
    term.select_update(crate::Point::new(line, 5));
    assert!(term.selection_range().is_some());
    term.resize(15, 6);
    assert!(term.selection_range().is_none());
}

#[test]
fn the_remap_follows_a_line_to_where_its_text_went() {
    let mut term = Term::new(20, 5);
    for n in 0..60 {
        term.feed(format!("N{n:03} aaaaaaaaaaaaaa\r\n").as_bytes());
    }
    // Find line "N030" before.
    let find = |term: &Term, tag: &str| {
        (term.first_line()..term.screen_top() + term.rows() as u64)
            .find(|&line| {
                let row = term.line(line).unwrap();
                row.cells.iter().take(4).map(|c| c.ch).collect::<String>() == tag
            })
            .unwrap()
    };
    let old = find(&term, "N030");
    let remap = term.resize(8, 5);
    check(&term);
    let new = remap.line(old);
    assert_eq!(new, find(&term, "N030"), "a line start maps to its first row");
    // The other way, widening.
    let remap = term.resize(40, 5);
    assert_eq!(remap.line(new), find(&term, "N030"));
}

/// A reflow of a full scrollback has to fit in a frame: `--ignored --nocapture`.
#[test]
#[ignore]
fn bench_reflow_of_10k_history_lines() {
    let mut term = Term::new(100, 40);
    term.set_scrollback(10_000);
    for n in 0..10_040 {
        term.feed(format!("{n:05} the quick brown fox jumps over the lazy dog {}\r\n", "z".repeat(n % 60)).as_bytes());
    }
    assert_eq!(term.scrollback_len(), 10_000);
    for (cols, rows) in [(60, 40), (100, 40), (30, 50), (100, 40)] {
        let start = std::time::Instant::now();
        term.resize(cols, rows);
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        println!("lazyterm reflow 10k lines to {cols}x{rows}: {ms:.2} ms");
    }
}
