//! Tests for the text buffer: line-index and character semantics, undo/redo
//! coalescing, dirty ranges and a large-file responsiveness bound.

use super::Buffer;

#[test]
fn every_ropey_line_break_is_excluded_from_line_content() {
    for terminator in [
        "\n", "\r\n", "\r", "\u{000B}", "\u{000C}", "\u{0085}", "\u{2028}", "\u{2029}",
    ] {
        let buffer = Buffer::new(&format!("ab{terminator}cd"));
        assert_eq!(buffer.line_count(), 2, "{terminator:?} splits lines");
        assert_eq!(buffer.line_string(0), "ab", "{terminator:?} is not content");
        assert_eq!(buffer.line_end(0), 2, "{terminator:?}: End stops before it");
        assert_eq!(buffer.max_line_chars(), 2, "{terminator:?} is not counted");
    }
}

#[test]
fn line_index_tracks_lines_including_a_trailing_empty_one() {
    let buffer = Buffer::new("one\ntwo\nthree");
    assert_eq!(buffer.line_count(), 3);
    assert_eq!(buffer.line_string(0), "one");
    assert_eq!(buffer.line_string(2), "three");
    assert_eq!(buffer.line_of_char(0), 0);
    assert_eq!(buffer.line_of_char(4), 1);
    assert_eq!(buffer.line_of_char(8), 2);

    let buffer = Buffer::new("a\n");
    assert_eq!(buffer.line_count(), 2);
    assert_eq!(buffer.line_string(1), "");
}

#[test]
fn crlf_terminators_are_not_part_of_the_line() {
    let buffer = Buffer::new("one\r\ntwo");
    assert_eq!(buffer.line_string(0), "one");
    assert_eq!(buffer.line_end(0), 3);
    assert_eq!(buffer.line_string(1), "two");
}

#[test]
fn insert_and_remove_update_the_text() {
    let mut buffer = Buffer::new("hello world");
    buffer.insert(5, ",", true);
    assert_eq!(buffer.text(), "hello, world");
    buffer.remove(5..6, true);
    assert_eq!(buffer.text(), "hello world");
}

#[test]
fn a_typing_run_coalesces_into_one_undo_group() {
    let mut buffer = Buffer::new("");
    for (at, ch) in "abc".chars().enumerate() {
        buffer.insert(at, &ch.to_string(), true);
    }
    assert_eq!(buffer.text(), "abc");
    assert_eq!(buffer.undo(), Some(0));
    assert_eq!(buffer.text(), "");
    assert_eq!(buffer.redo(), Some(3));
    assert_eq!(buffer.text(), "abc");
}

#[test]
fn non_adjacent_insertions_do_not_coalesce() {
    let mut buffer = Buffer::new("ab");
    buffer.insert(2, "c", true);
    buffer.insert(0, "x", true);
    assert_eq!(buffer.text(), "xabc");
    buffer.undo();
    assert_eq!(buffer.text(), "abc");
    buffer.undo();
    assert_eq!(buffer.text(), "ab");
}

#[test]
fn a_backspace_run_coalesces_into_one_group() {
    let mut buffer = Buffer::new("abcd");
    buffer.remove(3..4, true);
    buffer.remove(2..3, true);
    assert_eq!(buffer.text(), "ab");
    assert_eq!(buffer.undo(), Some(2));
    assert_eq!(buffer.text(), "abcd");
}

#[test]
fn a_forward_delete_run_coalesces_into_one_group() {
    let mut buffer = Buffer::new("abcd");
    buffer.remove(1..2, true);
    buffer.remove(1..2, true);
    assert_eq!(buffer.text(), "ad");
    assert_eq!(buffer.undo(), Some(1));
    assert_eq!(buffer.text(), "abcd");
}

#[test]
fn an_explicit_edit_is_one_undo_step() {
    let mut buffer = Buffer::new("a\nb\nc");
    buffer.begin_edit();
    buffer.insert(0, "  ", false);
    buffer.insert(4, "  ", false);
    buffer.insert(8, "  ", false);
    buffer.end_edit();
    assert_eq!(buffer.text(), "  a\n  b\n  c");
    buffer.undo();
    assert_eq!(buffer.text(), "a\nb\nc");
}

#[test]
fn the_line_index_follows_edits_inside_an_explicit_group() {
    let mut buffer = Buffer::new(
        "a
b",
    );
    buffer.begin_edit();
    buffer.insert(
        0, "x
", false,
    );
    assert_eq!(buffer.line_count(), 3);
    assert_eq!(buffer.line_start(2), 4);
    buffer.insert(buffer.line_start(2), "y", false);
    buffer.end_edit();
    assert_eq!(
        buffer.text(),
        "x
a
yb"
    );
}

#[test]
fn a_new_edit_clears_the_redo_stack() {
    let mut buffer = Buffer::new("a");
    buffer.insert(1, "b", true);
    buffer.undo();
    assert_eq!(buffer.text(), "a");
    buffer.insert(0, "z", true);
    assert_eq!(buffer.redo(), None);
    assert_eq!(buffer.text(), "za");
}

#[test]
fn multi_byte_chars_move_as_one_step() {
    let mut buffer = Buffer::new("héllo");
    assert_eq!(buffer.len_chars(), 5);
    buffer.remove(1..2, true);
    assert_eq!(buffer.text(), "hllo");
    buffer.insert(1, "é", true);
    assert_eq!(buffer.text(), "héllo");
    assert_eq!(buffer.line_of_char(buffer.len_chars()), 0);
}

#[test]
fn undo_reports_the_caret_it_restores() {
    let mut buffer = Buffer::new("hello");
    buffer.insert(5, " world", true);
    assert_eq!(buffer.undo(), Some(5));
    assert_eq!(buffer.redo(), Some(11));
}

#[test]
fn the_revision_moves_only_when_the_text_changes() {
    let mut buffer = Buffer::new("hello");
    assert!(!buffer.can_undo() && !buffer.can_redo());
    let start = buffer.revision();
    buffer.insert(5, "", false);
    buffer.remove(2..2, false);
    assert_eq!(buffer.revision(), start, "empty edits change nothing");
    assert_eq!(buffer.undo(), None);
    assert_eq!(buffer.revision(), start, "an empty undo changes nothing");

    buffer.insert(5, "!", false);
    assert_ne!(buffer.revision(), start);
    assert!(buffer.can_undo() && !buffer.can_redo());

    let edited = buffer.revision();
    buffer.undo();
    assert_ne!(buffer.revision(), edited, "undo changes the text");
    assert!(!buffer.can_undo() && buffer.can_redo());
    let undone = buffer.revision();
    buffer.redo();
    assert_ne!(buffer.revision(), undone, "redo changes the text");
    assert_eq!(buffer.redo(), None);
}

#[test]
fn edits_report_the_changed_range_and_clear_it() {
    let mut buffer = Buffer::new("one\ntwo\nthree");
    assert_eq!(buffer.take_dirty(), None);

    // Two inserts: the range spans both, the later one shifted by the
    // earlier ("one\n!two\n!three": 4..10).
    let late = buffer.line_start(2);
    let early = buffer.line_start(1);
    buffer.insert(late, "!", true);
    buffer.insert(early, "!", true);
    assert_eq!(buffer.take_dirty(), Some(early..late + 2));
    assert_eq!(buffer.take_dirty(), None);

    buffer.remove(0..1, true);
    assert_eq!(buffer.take_dirty(), Some(0..0));

    // A replacement covers everything it inserted.
    buffer.replace(0..2, "abc\ndef", false);
    assert_eq!(buffer.take_dirty(), Some(0..7));

    let end = buffer.len_chars();
    buffer.insert(end, "x", true);
    assert_eq!(buffer.take_dirty(), Some(end..end + 1));
    buffer.undo();
    assert_eq!(buffer.take_dirty(), Some(end..end));
    buffer.redo();
    assert_eq!(buffer.take_dirty(), Some(end..end + 1));
}

#[test]
fn the_longest_line_is_tracked() {
    let mut buffer = Buffer::new("a\nlonger\nbb");
    assert_eq!(buffer.max_line_chars(), 6);
    buffer.insert(0, "xxxxxxx\n", false);
    assert_eq!(buffer.max_line_chars(), 7);
}

#[test]
fn the_widest_line_counts_tabs_as_display_columns() {
    let mut buffer = Buffer::new("\t\tx\nabcdef");
    assert_eq!(buffer.max_line_chars(), 6);
    assert_eq!(buffer.max_line_cols(4), 9);
    assert_eq!(buffer.max_line_cols(8), 17);
    buffer.insert(0, "\t", false);
    assert_eq!(buffer.max_line_cols(4), 13, "an edit invalidates the cache");
}

#[test]
fn a_five_thousand_line_file_stays_responsive_to_edits() {
    // 5,000 lines of Rhai-ish text, then 1,000 single-character inserts at
    // the end. If any step were quadratic this would take minutes; the
    // generous bound only catches that, not normal machine variance.
    let mut text = String::new();
    for line in 0..5_000 {
        text.push_str("fn handler_");
        text.push_str(&line.to_string());
        text.push_str("() { let x = 1; }\n");
    }
    let mut buffer = Buffer::new(&text);
    assert_eq!(buffer.line_count(), 5_001);

    let started = std::time::Instant::now();
    let base = buffer.len_chars();
    for caret in base..base + 1_000 {
        buffer.insert(caret, "x", true);
    }
    assert_eq!(buffer.len_chars(), text.chars().count() + 1_000);
    buffer.undo();
    assert_eq!(buffer.len_chars(), text.chars().count());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "1,000 edits on a 5,000-line file took {:?}",
        started.elapsed()
    );
}
