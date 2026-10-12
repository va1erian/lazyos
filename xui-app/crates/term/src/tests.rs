//! Behaviour tests: the old grid's, then a sequence -> expected screen table
//! (the `vttest` idea at unit scale), then wide characters, attributes,
//! replies and the logical-line evidence.

use crate::fuzz::check;
use crate::{Color, Flags, Term};

/// The screen as trimmed row texts.
fn screen(term: &Term) -> Vec<String> {
    (0..term.rows()).map(|row| term.row_text(row)).collect()
}

fn run(cols: usize, rows: usize, input: &[u8]) -> Term {
    let mut term = Term::new(cols, rows);
    term.feed(input);
    check(&term);
    term
}

// ---- the old grid's tests -----------------------------------------------

#[test]
fn tab_in_last_column_then_erase_does_not_panic() {
    let mut term = Term::new(80, 24);
    term.feed("x".repeat(80).as_bytes());
    term.feed(b"\t\x1b[K\x1b[J");
    assert!(term.cursor().1 < 80);
    check(&term);
}

/// `ESC [ J` clears from the cursor to the end of the *screen*, the rows
/// below included (full-screen programs redraw with `ESC [H ESC [J`).
#[test]
fn erase_below_clears_the_rows_under_the_cursor() {
    let mut term = Term::new(80, 24);
    term.feed(b"top\r\nmiddle\r\nbottom");
    term.feed(b"\x1b[2;3H\x1b[J");
    assert_eq!(term.row_text(0), "top");
    assert_eq!(term.row_text(1), "mi");
    assert_eq!(term.row_text(2), "");
}

/// The alternate screen keeps the shell's screen and cursor intact.
#[test]
fn alternate_screen_restores_the_main_screen() {
    let mut term = Term::new(80, 24);
    term.feed(b"$ less file");
    term.feed(b"\x1b[?1049h\x1b[Hpager text\x1b[5;1H:");
    assert!(term.on_alternate_screen());
    assert_eq!(term.row_text(0), "pager text");
    term.feed(b"\x1b[?1049l");
    assert!(!term.on_alternate_screen());
    assert_eq!(term.row_text(0), "$ less file");
    assert_eq!(term.cursor(), (0, 11));
}

#[test]
fn cursor_position_uses_both_parameters() {
    let mut term = Term::new(80, 24);
    term.feed(b"\x1b[5;10H");
    assert_eq!(term.cursor(), (4, 9));
}

// ---- sequence -> expected screen ----------------------------------------

struct Case {
    name: &'static str,
    size: (usize, usize),
    input: &'static str,
    screen: &'static [&'static str],
    cursor: (usize, usize),
}

const fn case(
    name: &'static str,
    size: (usize, usize),
    input: &'static str,
    screen: &'static [&'static str],
    cursor: (usize, usize),
) -> Case {
    Case {
        name,
        size,
        input,
        screen,
        cursor,
    }
}

const TABLE: &[Case] = &[
    case("text and CRLF", (8, 3), "ab\r\ncd", &["ab", "cd", ""], (1, 2)),
    case("LF keeps the column", (8, 3), "ab\ncd", &["ab", "  cd", ""], (1, 4)),
    case("LNM makes LF return the carriage", (8, 3), "\x1b[20hab\ncd\x1b[20l\nef", &["ab", "cd", "  ef"], (2, 4)),
    case("backspace overwrites", (8, 2), "abc\x08\x08X", &["aXc", ""], (0, 2)),
    case("CUP", (8, 4), "\x1b[3;4Hx", &["", "", "   x", ""], (2, 4)),
    case("CUU CUD CUF CUB", (8, 4), "\x1b[3;3H\x1b[2A\x1b[3C\x1b[1B\x1b[2Dx", &["", "   x", "", ""], (1, 4)),
    case("CHA and VPA", (8, 4), "\x1b[4d\x1b[5Gx", &["", "", "", "    x"], (3, 5)),
    case("cursor moves clamp", (4, 3), "\x1b[99;99Hx", &["", "", "   x"], (2, 3)),
    case("ED 2 keeps the cursor", (8, 3), "abc\r\ndef\x1b[2J", &["", "", ""], (1, 3)),
    case("ED 1", (8, 3), "abc\r\ndef\r\nghi\x1b[2;2H\x1b[1J", &["", "  f", "ghi"], (1, 1)),
    case("EL 0", (8, 1), "abcdef\x1b[1;3H\x1b[K", &["ab"], (0, 2)),
    case("EL 1", (8, 1), "abcdef\x1b[1;3H\x1b[1K", &["   def"], (0, 2)),
    case("EL 2", (8, 1), "abcdef\x1b[2K", &[""], (0, 6)),
    case("ICH", (6, 1), "abcdef\x1b[1;3H\x1b[2@", &["ab  cd"], (0, 2)),
    case("DCH", (6, 1), "abcdef\x1b[1;3H\x1b[2P", &["abef"], (0, 2)),
    case("ECH", (6, 1), "abcdef\x1b[1;3H\x1b[2X", &["ab  ef"], (0, 2)),
    case("IL", (4, 4), "a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[L", &["a", "", "b", "c"], (1, 0)),
    case("DL", (4, 4), "a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[M", &["a", "c", "d", ""], (1, 0)),
    case("IL outside the region does nothing", (4, 4), "a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[1;1H\x1b[L", &["a", "b", "c", "d"], (0, 0)),
    case("scroll region", (4, 5), "1\r\n2\r\n3\r\n4\r\n5\x1b[2;4r\x1b[4;1H\n", &["1", "3", "4", "", "5"], (3, 0)),
    case("scroll region DL", (4, 5), "1\r\n2\r\n3\r\n4\r\n5\x1b[2;4r\x1b[2;1H\x1b[M", &["1", "3", "4", "", "5"], (1, 0)),
    case("DECSTBM homes the cursor", (4, 5), "\x1b[4;3Hx\x1b[2;4r", &["", "", "", "  x", ""], (0, 0)),
    case("SU and SD", (4, 3), "a\r\nb\r\nc\x1b[S", &["b", "c", ""], (2, 1)),
    case("SD", (4, 3), "a\r\nb\r\nc\x1b[T", &["", "a", "b"], (2, 1)),
    case("IND scrolls at the bottom", (4, 2), "a\r\nb\x1bD", &["b", ""], (1, 1)),
    case("RI scrolls down at the top", (4, 2), "a\r\nb\x1b[H\x1bM", &["", "a"], (0, 0)),
    case("RI inside the region", (4, 4), "a\r\nb\r\nc\r\nd\x1b[2;3r\x1b[2;1H\x1bM", &["a", "", "b", "d"], (1, 0)),
    case("NEL", (4, 3), "ab\x1bEc", &["ab", "c", ""], (1, 1)),
    case("DECSC DECRC", (6, 2), "ab\x1b7\r\nxyz\x1b8Q", &["abQ", "xyz"], (0, 3)),
    case("CSI s u", (6, 2), "ab\x1b[s\r\nxy\x1b[uQ", &["abQ", "xy"], (0, 3)),
    case("restore without save homes", (6, 2), "ab\r\ncd\x1b8x", &["xb", "cd"], (0, 1)),
    case("default tab stops", (20, 1), "a\tb\tc", &["a       b       c"], (0, 17)),
    case("HTS", (20, 1), "\x1b[3g\x1b[1;5H\x1bH\x1b[1;1H\tX", &["    X"], (0, 5)),
    case("TBC 0", (20, 1), "\x1b[1;9H\x1b[g\x1b[1;1H\tX", &["                X"], (0, 17)),
    case("CHT and CBT", (20, 1), "\x1b[2I\x1b[1ZX", &["        X"], (0, 9)),
    case("tab stops end at the margin", (10, 1), "\t\t\tX", &["         X"], (0, 9)),
    case("autowrap", (5, 3), "abcdefg", &["abcde", "fg", ""], (1, 2)),
    case("deferred wrap: no blank line", (5, 3), "abcde\r\nX", &["abcde", "X", ""], (1, 1)),
    case("CR at the margin stays on the row", (5, 2), "abcde\rXY", &["XYcde", ""], (0, 2)),
    case("DECAWM off overwrites the margin", (5, 2), "\x1b[?7labcdefghij", &["abcdj", ""], (0, 4)),
    case("DECAWM back on", (3, 3), "\x1b[?7l\x1b[?7habcd", &["abc", "d", ""], (1, 1)),
    case("wrap scrolls the screen", (3, 2), "abcdefgh", &["def", "gh"], (1, 2)),
    case("DECALN", (3, 2), "\x1b#8", &["EEE", "EEE"], (0, 0)),
    case("RIS", (4, 2), "ab\x1b[2;2r\x1b[?7l\x1bc", &["", ""], (0, 0)),
    case("OSC title is swallowed", (8, 1), "a\x1b]0;a title\x07b", &["ab"], (0, 2)),
    case("OSC with ST is swallowed", (8, 1), "a\x1b]2;t\x1b\\b", &["ab"], (0, 2)),
    case("DCS is swallowed", (8, 1), "a\x1bPq#0;2;0\x1b\\b", &["ab"], (0, 2)),
    case("charset designators are ignored", (8, 1), "a\x1b(0b\x1b)Bc", &["abc"], (0, 3)),
    case("unknown CSI is ignored", (8, 1), "a\x1b[5;7;9~\x1b[?9999zb", &["ab"], (0, 2)),
    case("BEL and other controls print nothing", (8, 1), "a\x07\x00\x0e\x0fb", &["ab"], (0, 2)),
    case("huge parameters clamp", (8, 2), "a\x1b[99999999999;99999999999Hb", &["a", "       b"], (1, 7)),
];

#[test]
fn sequences_produce_the_expected_screens() {
    for case in TABLE {
        let term = run(case.size.0, case.size.1, case.input.as_bytes());
        assert_eq!(screen(&term), case.screen, "{}: screen", case.name);
        assert_eq!(term.cursor(), case.cursor, "{}: cursor", case.name);
    }
}

#[test]
fn the_table_covers_every_row() {
    for case in TABLE {
        assert_eq!(case.screen.len(), case.size.1, "{}: expected rows", case.name);
    }
}

#[test]
fn dectcem_hides_and_shows_the_cursor() {
    let mut term = Term::new(8, 2);
    assert!(term.cursor_visible());
    term.feed(b"\x1b[?25l");
    assert!(!term.cursor_visible());
    term.feed(b"\x1b[?25h");
    assert!(term.cursor_visible());
}

#[test]
fn a_split_escape_sequence_and_a_split_utf8_character_survive() {
    let mut term = Term::new(8, 1);
    for chunk in [&b"\x1b"[..], b"[", b"1;3", b"H\xe4", b"\xb8", b"\xad"] {
        term.feed(chunk);
    }
    assert_eq!(term.row_text(0), "  \u{4e2d}");
}

#[test]
fn alternate_screen_modes_47_and_1047_work_too() {
    for mode in ["47", "1047", "1049"] {
        let mut term = Term::new(8, 2);
        term.feed(format!("main\x1b[?{mode}hALT\x1b[?{mode}l").as_bytes());
        assert_eq!(term.row_text(0), "main", "mode {mode}");
    }
}

#[test]
fn the_alternate_screen_starts_blank_and_has_no_margins_of_the_main_one() {
    let mut term = Term::new(8, 4);
    term.feed(b"abc\x1b[2;3r\x1b[?1049h");
    assert_eq!(screen(&term), ["", "", "", ""]);
    term.feed(b"\x1b[4;1H\nx");
    assert_eq!(term.row_text(3), "x");
    assert_eq!(term.row_text(2), "", "the whole screen scrolled, no region");
}

// ---- wide characters ------------------------------------------------------

#[test]
fn wide_characters_take_two_cells() {
    let term = run(6, 1, "a\u{4e2d}b".as_bytes());
    let cells = &term.row(0).cells;
    assert_eq!((cells[1].ch, cells[1].width), ('\u{4e2d}', 2));
    assert_eq!(cells[2].width, 0, "the spacer");
    assert_eq!((cells[3].ch, term.cursor().1), ('b', 4));
    assert_eq!(term.row_text(0), "a\u{4e2d}b");
}

#[test]
fn a_wide_character_that_does_not_fit_wraps_with_a_pad() {
    let term = run(5, 2, "abcd\u{4e2d}".as_bytes());
    assert_eq!(screen(&term), ["abcd", "\u{4e2d}"]);
    assert!(term.row(0).wrapped);
    assert_eq!(term.row(0).cells[4].width, 0, "the pad");
    assert_eq!(term.logical_line(1), "abcd\u{4e2d}");
}

#[test]
fn wide_character_exactly_at_the_margin_defers_the_wrap() {
    let mut term = run(4, 2, "ab\u{4e2d}".as_bytes());
    assert_eq!(term.cursor(), (0, 3));
    term.feed(b"c");
    assert_eq!(screen(&term), ["ab\u{4e2d}", "c"]);
}

#[test]
fn erasing_half_a_wide_character_erases_both() {
    // The spacer (column 1) is erased: the head goes too.
    let term = run(6, 1, "\u{4e2d}\u{4e2d}\x1b[1;2H\x1b[X".as_bytes());
    assert_eq!(term.row_text(0), "  \u{4e2d}");
    // The head (column 2) is erased: the spacer goes too.
    let term = run(6, 1, "\u{4e2d}\u{4e2d}\x1b[1;3H\x1b[X".as_bytes());
    assert_eq!(term.row_text(0), "\u{4e2d}");
}

#[test]
fn overwriting_half_a_wide_character_blanks_the_other_half() {
    let term = run(6, 1, "\u{4e2d}\x1b[1;2Hx".as_bytes());
    assert_eq!(term.row_text(0), " x");
    let term = run(6, 1, "\u{4e2d}\x1b[1;1Hxy".as_bytes());
    assert_eq!(term.row_text(0), "xy");
}

#[test]
fn insert_and_delete_never_leave_half_a_wide_character() {
    let term = run(6, 1, "a\u{4e2d}b\x1b[1;1H\x1b[P".as_bytes());
    assert_eq!(term.row_text(0), "\u{4e2d}b");
    let term = run(6, 1, "a\u{4e2d}b\x1b[1;1H\x1b[2P".as_bytes());
    assert_eq!(term.row_text(0), " b", "the head went, so its spacer is a blank");
    let term = run(4, 1, "ab\u{4e2d}\x1b[1;1H\x1b[@".as_bytes());
    assert_eq!(term.row_text(0), " ab", "pushed past the margin, the head is dropped");
}

#[test]
fn cursor_movement_over_wide_characters_counts_cells() {
    let term = run(6, 1, "\u{4e2d}\u{4e2d}\x1b[2D".as_bytes());
    assert_eq!(term.cursor(), (0, 2));
}

#[test]
fn zero_width_characters_take_no_cell() {
    let term = run(6, 1, "e\u{301}x".as_bytes());
    assert_eq!(term.row_text(0), "ex");
}

// ---- attributes -------------------------------------------------------------

#[test]
fn sgr_is_stored_per_cell() {
    let term = run(8, 1, b"\x1b[1;31mA\x1b[0mB\x1b[4;7mC\x1b[24mD\x1b[mE");
    let attr = |col: usize| term.row(0).cells[col].attr;
    assert_eq!(attr(0).fg, Color::Indexed(1));
    assert!(attr(0).flags.has(Flags::BOLD));
    assert_eq!(attr(1), Default::default());
    assert!(attr(2).flags.has(Flags::UNDERLINE) && attr(2).flags.has(Flags::INVERSE));
    assert!(!attr(3).flags.has(Flags::UNDERLINE) && attr(3).flags.has(Flags::INVERSE));
    assert_eq!(attr(4), Default::default());
}

#[test]
fn extended_colours_in_both_forms() {
    let term = run(
        8,
        1,
        b"\x1b[38;5;200mA\x1b[48;2;1;2;3mB\x1b[38:5:17mC\x1b[48:2::9:8:7mD\x1b[38:2:4:5:6mE\x1b[39;49mF\x1b[91;102mG",
    );
    let attr = |col: usize| term.row(0).cells[col].attr;
    assert_eq!(attr(0).fg, Color::Indexed(200));
    assert_eq!(attr(1).bg, Color::Rgb(1, 2, 3));
    assert_eq!(attr(1).fg, Color::Indexed(200));
    assert_eq!(attr(2).fg, Color::Indexed(17));
    assert_eq!(attr(3).bg, Color::Rgb(9, 8, 7));
    assert_eq!(attr(4).fg, Color::Rgb(4, 5, 6));
    assert_eq!((attr(5).fg, attr(5).bg), (Color::Default, Color::Default));
    assert_eq!((attr(6).fg, attr(6).bg), (Color::Indexed(9), Color::Indexed(10)));
}

#[test]
fn truncated_extended_colours_are_ignored() {
    let term = run(4, 1, b"\x1b[38;2;1mA\x1b[48;5mB\x1b[38mC");
    assert_eq!(term.row(0).cells[2].attr, Default::default());
}

#[test]
fn erased_cells_keep_only_the_background() {
    let term = run(6, 1, b"\x1b[1;31;44mabc\x1b[1;1H\x1b[K");
    let cell = term.row(0).cells[0];
    assert_eq!((cell.ch, cell.attr.bg, cell.attr.fg), (' ', Color::Indexed(4), Color::Default));
    assert!(!cell.attr.flags.has(Flags::BOLD));
}

#[test]
fn decsc_saves_the_attributes_too() {
    let term = run(6, 1, b"\x1b[31m\x1b7\x1b[0m\x1b8x");
    assert_eq!(term.row(0).cells[0].attr.fg, Color::Indexed(1));
}

// ---- replies ----------------------------------------------------------------

#[test]
fn status_reports_are_queued_for_the_pty() {
    let mut term = Term::new(80, 24);
    term.feed(b"\x1b[3;5H\x1b[6n");
    assert_eq!(term.take_reply(), b"\x1b[3;5R");
    assert!(term.take_reply().is_empty());
    term.feed(b"\x1b[5n\x1b[c\x1b[0c");
    assert_eq!(term.take_reply(), b"\x1b[0n\x1b[?6c\x1b[?6c");
}

#[test]
fn a_cursor_report_at_the_margin_is_in_range() {
    let mut term = Term::new(10, 3);
    term.feed(b"0123456789\x1b[6n");
    assert_eq!(term.take_reply(), b"\x1b[1;10R");
}

#[test]
fn unsafe_queries_get_no_answer() {
    let mut term = Term::new(80, 24);
    term.feed(b"\x1b[21t\x1b[18t\x1b]0;?\x07\x1b[>c\x1bP$qm\x1b\\");
    assert!(term.take_reply().is_empty());
}

// ---- logical lines (the evidence) ---------------------------------------------

/// The completed lines' texts.
fn lines(term: &mut Term, input: &[u8]) -> Vec<String> {
    term.feed(input).into_iter().map(|line| line.text).collect()
}

#[test]
fn completed_lines_skip_blank_ones() {
    let mut term = Term::new(20, 5);
    assert_eq!(lines(&mut term, b"a\r\n\r\nb  \r\n"), ["a", "b"]);
    assert_eq!(lines(&mut term, b"par"), Vec::<String>::new());
    assert_eq!(lines(&mut term, b"tial\r\n"), ["partial"]);
}

#[test]
fn a_wrapped_command_echo_is_one_logical_line() {
    let command = "doom -timedemo demo1 -nosound -nomusic -warp 1 1";
    let mut term = Term::new(24, 6);
    let got = lines(&mut term, format!("/ # {command}\r\nout\r\n").as_bytes());
    assert_eq!(got, [format!("/ # {command}"), "out".to_string()]);
    assert!(got[0].ends_with(command));
}

#[test]
fn a_logical_line_survives_its_head_scrolling_off() {
    let mut term = Term::new(5, 2);
    assert_eq!(lines(&mut term, b"abcdefghijkl\r\nz\r\n"), ["abcdefghijkl", "z"]);
}

#[test]
fn a_line_that_was_overwritten_is_reported_as_it_looks() {
    // Line editing redraws with CR and overwrites: the screen is the truth.
    let mut term = Term::new(20, 2);
    assert_eq!(lines(&mut term, b"/ # ech\r/ # echo\r\n"), ["/ # echo"]);
}

#[test]
fn lines_completed_on_the_alternate_screen_are_reported() {
    let mut term = Term::new(20, 3);
    assert_eq!(lines(&mut term, b"\x1b[?1049hhi\r\n"), ["hi"]);
}

/// BusyBox's line editor ends a full row with its own `CR CR LF`, no
/// autowrap: the engine sees two lines and says the first reached the margin.
#[test]
fn a_row_broken_by_hand_at_the_margin_is_flagged() {
    let mut term = Term::new(5, 4);
    let got = term.feed(b"abcde\r\r\nfg\r\nabcd\r\n");
    let seen: Vec<_> = got
        .iter()
        .map(|line| (line.text.as_str(), line.ends_at_margin))
        .collect();
    assert_eq!(seen, [("abcde", true), ("fg", false), ("abcd", false)]);
    assert!(!term.row(0).wrapped, "a hand-made break is not a wrap");
    let wrapped = term.feed(b"abcdefg\r\n");
    assert_eq!(wrapped.len(), 1);
    assert_eq!((wrapped[0].text.as_str(), wrapped[0].ends_at_margin), ("abcdefg", false));
}

// ---- size ---------------------------------------------------------------------

#[test]
fn resize_rewraps_pads_and_keeps_the_cursor_on_screen() {
    let mut term = Term::new(8, 4);
    term.feed(b"abcdefgh\r\nij\r\nkl\r\nmn");
    term.resize(4, 2);
    check(&term);
    assert_eq!(screen(&term), ["kl", "mn"]);
    assert_eq!(term.cursor(), (1, 2));
    term.resize(10, 5);
    check(&term);
    assert_eq!((term.cols(), term.rows()), (10, 5));
    // The wrapped line joins again, and the scrolled-off rows come back.
    assert_eq!(screen(&term), ["abcdefgh", "ij", "kl", "mn", ""]);
    assert_eq!(term.cursor(), (3, 2));
    term.feed(b"\x1b[1;10H\x1b[Zx");
    check(&term);
}

#[test]
fn resize_extends_the_tab_stops_and_never_splits_wide_characters() {
    let mut term = Term::new(4, 1);
    term.feed("ab\u{4e2d}".as_bytes());
    term.resize(3, 1);
    check(&term);
    // The wide character wrapped whole onto the row below; the last row shows.
    assert_eq!(term.row_text(0), "中");
    assert_eq!(term.scrollback_len(), 1);
    term.resize(20, 1);
    term.feed(b"\r\t\tX");
    assert_eq!(term.cursor(), (0, 17));
}

#[test]
fn resize_on_the_alternate_screen_resizes_the_main_one_too() {
    let mut term = Term::new(8, 3);
    term.feed(b"main\x1b[?1049h");
    term.resize(5, 2);
    check(&term);
    term.feed(b"\x1b[?1049l");
    check(&term);
    assert_eq!(term.row_text(0), "main");
}

#[test]
fn the_size_is_clamped() {
    let term = Term::new(0, 0);
    assert_eq!((term.cols(), term.rows()), (1, 1));
    let mut term = Term::new(100_000, 100_000);
    assert_eq!((term.cols(), term.rows()), (crate::MAX_COLS, crate::MAX_ROWS));
    term.resize(0, 0);
    assert_eq!((term.cols(), term.rows()), (1, 1));
}

#[test]
fn a_one_cell_terminal_survives_everything() {
    let mut term = Term::new(1, 1);
    term.feed("\u{4e2d}abc\r\n\x1b[L\x1b[M\x1b[@\x1b[P\x1b[S\x1b[T\x1bD\x1bM\x1b[2;3r\t".as_bytes());
    check(&term);
}

// ---- throughput -----------------------------------------------------------------

/// A sustained-load check: a megabyte of output through a small screen.
#[test]
fn a_megabyte_of_output_keeps_the_screen_consistent() {
    let mut term = Term::new(80, 24);
    let line = "0123456789 the quick brown fox jumps over the lazy dog 0123456789\r\n";
    let chunk = line.repeat(64);
    let mut lines = 0;
    for _ in 0..(1 << 20) / chunk.len() + 1 {
        lines += term.feed(chunk.as_bytes()).len();
    }
    assert!(lines > 15_000);
    check(&term);
}
