//! The modes and OSC commands of T5: application cursor and keypad, focus
//! reporting, synchronized output, device attributes, titles, working
//! directories and hyperlinks, with hostile input.

use crate::fuzz::check;
use crate::keys::KeyModes;
use crate::Term;

fn term() -> Term {
    Term::new(40, 6)
}

#[test]
fn application_cursor_and_keypad_modes_are_set_and_cleared() {
    let mut t = term();
    assert_eq!(t.key_modes(), KeyModes { app_cursor: false, app_keypad: false });
    t.feed(b"\x1b[?1h");
    assert!(t.key_modes().app_cursor && !t.key_modes().app_keypad);
    t.feed(b"\x1b=");
    assert!(t.key_modes().app_keypad);
    t.feed(b"\x1b[?1l\x1b>");
    assert_eq!(t.key_modes(), KeyModes::default());
    // Several modes in one sequence, with an unknown one between.
    t.feed(b"\x1b[?1;9999;1004h");
    assert!(t.key_modes().app_cursor && t.focus_reporting());
    // A full reset puts them back.
    t.feed(b"\x1b=\x1bc");
    assert_eq!(t.key_modes(), KeyModes::default());
    assert!(!t.focus_reporting());
}

#[test]
fn focus_reports_only_when_asked() {
    let mut t = term();
    assert_eq!(t.focus_report(true), None);
    assert_eq!(t.focus_report(false), None);
    t.feed(b"\x1b[?1004h");
    assert_eq!(t.focus_report(true), Some(&b"\x1b[I"[..]));
    assert_eq!(t.focus_report(false), Some(&b"\x1b[O"[..]));
    t.feed(b"\x1b[?1004l");
    assert_eq!(t.focus_report(true), None);
}

#[test]
fn synchronized_output_is_a_flag_the_host_holds_paints_for() {
    let mut t = term();
    assert!(!t.sync_output());
    t.feed(b"\x1b[?2026h");
    assert!(t.sync_output());
    t.feed(b"drawing...");
    assert!(t.sync_output(), "still inside the update");
    t.feed(b"\x1b[?2026l");
    assert!(!t.sync_output());
    // A program that never ends it leaves it on: the host's timeout is the
    // safety net, and the engine keeps working meanwhile.
    t.feed(b"\x1b[?2026hmore text");
    assert!(t.sync_output());
    assert_eq!(t.row_text(0), "drawing...more text");
    // The alternate screen does not end it, and neither does a resize.
    t.feed(b"\x1b[?1049h");
    t.resize(30, 4);
    assert!(t.sync_output());
    check(&t);
}

#[test]
fn device_attributes_are_a_vt220_with_colour() {
    let mut t = term();
    t.feed(b"\x1b[c\x1b[0c");
    assert_eq!(t.take_reply(), b"\x1b[?62;22c\x1b[?62;22c");
    t.feed(b"\x1b[>c\x1b[>0c");
    assert_eq!(t.take_reply(), b"\x1b[>0;10;0c\x1b[>0;10;0c");
    // Other parameters are not queries this terminal answers.
    t.feed(b"\x1b[1c\x1b[>1c\x1b[=c");
    assert_eq!(t.take_reply(), b"");
}

#[test]
fn titles_from_osc_0_and_2() {
    let mut t = term();
    assert_eq!(t.title(), None);
    let before = t.title_generation();
    t.feed(b"\x1b]0;hello title\x07");
    assert_eq!(t.title(), Some("hello title"));
    assert!(t.title_generation() > before);
    let after = t.title_generation();
    t.feed(b"\x1b]2;hello title\x1b\\");
    assert_eq!(t.title_generation(), after, "the same title is not a change");
    t.feed(b"\x1b]2;a;b;c\x07");
    assert_eq!(t.title(), Some("a;b;c"), "semicolons belong to the title");
    t.feed(b"\x1b]1;icon only\x07");
    assert_eq!(t.title(), Some("a;b;c"), "OSC 1 names the icon");
    t.feed(b"\x1b]2;\x07");
    assert_eq!(t.title(), None, "an empty title clears it");
    t.feed(b"\x1b]0;again\x07\x1bc");
    assert_eq!(t.title(), None, "a reset clears it");
    assert!(t.title_generation() > after);
    // The text of the OSC is not drawn.
    assert_eq!(t.row_text(0), "");
}

#[test]
fn hostile_titles_are_bounded_and_clean() {
    let mut t = term();
    let mut huge = b"\x1b]0;".to_vec();
    huge.extend(std::iter::repeat_n(b'x', 100_000));
    huge.push(7);
    t.feed(&huge);
    assert!(t.title().is_none_or(|title| title.chars().count() <= crate::MAX_TITLE));
    // Controls inside are dropped, never kept.
    t.feed(b"\x1b]2;a\x01b\x7fc\xc2\x85d\x07");
    let title = t.title().unwrap();
    assert!(!title.chars().any(|c| c.is_control()), "{title:?}");
    // Not UTF-8, and no terminator at all: nothing is kept or broken.
    t.feed(b"\x1b]2;\xff\xfe\x07");
    t.feed(b"\x1b]2;never ends");
    check(&t);
    t.feed(b"\x1b\\ok");
    assert_eq!(t.row_text(0), "ok");
}

#[test]
fn the_working_directory_from_osc_7() {
    let mut t = term();
    assert!(t.cwd().is_none());
    t.feed(b"\x1b]7;file://lazyos/srv/my%20dir\x07");
    let cwd = t.cwd().unwrap();
    assert_eq!((cwd.host.as_str(), cwd.path.as_str()), ("lazyos", "/srv/my dir"));
    t.feed(b"\x1b]7;file:///\x1b\\");
    assert_eq!(t.cwd().unwrap().path, "/");
    // A bad one leaves the last good one alone.
    for bad in [&b"\x1b]7;http://x/y\x07"[..], b"\x1b]7;file:///a%zz\x07", b"\x1b]7;\x07", b"\x1b]7;file:///a%0Ab\x07"] {
        t.feed(bad);
        assert_eq!(t.cwd().unwrap().path, "/", "{:?}", String::from_utf8_lossy(bad));
    }
    let mut huge = b"\x1b]7;file:///".to_vec();
    huge.extend(std::iter::repeat_n(b'a', 50_000));
    huge.push(7);
    t.feed(&huge);
    assert!(t.cwd().unwrap().path.len() <= crate::osc_meta::MAX_CWD);
    check(&t);
}

#[test]
fn hyperlinks_mark_cells_until_they_end() {
    let mut t = term();
    t.feed(b"a\x1b]8;;https://example.com/x\x07link\x1b]8;;\x07 b");
    let line = t.screen_top();
    assert_eq!(t.link_at(line, 0), None);
    for col in 1..=4 {
        assert_eq!(t.link_at(line, col), Some("https://example.com/x"), "col {col}");
    }
    assert_eq!(t.link_at(line, 5), None);
    assert_eq!(t.links_on(line), [(1, 4, "https://example.com/x")]);
    assert_eq!(t.row_text(0), "alink b");
}

#[test]
fn a_link_survives_sgr_and_ends_only_with_osc_8() {
    let mut t = term();
    t.feed(b"\x1b]8;id=1;http://a\x1b\\x\x1b[1;31my\x1b[0mz\x1b]8;;\x1b\\w");
    let line = t.screen_top();
    assert_eq!(t.links_on(line), [(0, 2, "http://a")]);
    // Colours and attributes are untouched by the link.
    assert!(t.line(line).unwrap().cells[1].attr.flags.has(crate::Flags::BOLD));
    assert_eq!(t.link_at(line, 3), None);
}

#[test]
fn links_are_interned_and_different_ones_stay_apart() {
    let mut t = term();
    t.feed(b"\x1b]8;;http://a\x07a\x1b]8;;\x07\x1b]8;;http://b\x07b\x1b]8;;\x07\x1b]8;;http://a\x07a\x1b]8;;\x07");
    let line = t.screen_top();
    let spans = t.links_on(line);
    assert_eq!(spans, [(0, 0, "http://a"), (1, 1, "http://b"), (2, 2, "http://a")]);
    assert_eq!(t.state.links.len(), 2, "the repeated link is one entry");
    // The same URI under another id is its own link (they are separate targets).
    t.feed(b"\x1b]8;id=x;http://a\x07q\x1b]8;;\x07");
    assert_eq!(t.state.links.len(), 3);
}

#[test]
fn an_erase_drops_the_link_and_a_scroll_keeps_it_with_its_text() {
    let mut t = Term::new(10, 2);
    t.feed(b"\x1b]8;;http://a\x07link\x1b]8;;\x07");
    let line = t.screen_top();
    assert!(t.link_at(line, 0).is_some());
    t.feed(b"\x1b[2K");
    assert_eq!(t.link_at(line, 0), None);
    t.feed(b"\x1b[1;1H\x1b]8;;http://a\x07link\x1b]8;;\x07\r\n\r\n\r\n");
    // The linked row scrolled into the history with its link.
    assert_eq!(t.link_at(t.first_line(), 0), Some("http://a"));
    // An erase with a link active does not paint blanks with it.
    t.feed(b"\x1b]8;;http://b\x07\x1b[2K");
    assert_eq!(t.link_at(t.screen_top(), 0), None);
}

#[test]
fn hostile_links_are_refused_or_bounded() {
    let mut t = term();
    // Too long a URI is no link, not a cut one.
    let mut long = b"\x1b]8;;http://".to_vec();
    long.extend(std::iter::repeat_n(b'a', 5000));
    long.extend_from_slice(b"\x07x\x1b]8;;\x07");
    t.feed(&long);
    assert_eq!(t.link_at(t.screen_top(), 0), None);
    // Control characters never reach the table: the parser drops them from the
    // OSC, and what is stored is checked again.
    t.feed(b"\r\x1b]8;;http://a\x01b\x07y\x1b]8;;\x07");
    let uri = t.link_at(t.screen_top(), 0).unwrap_or("");
    assert!(!uri.chars().any(|c| c.is_control()), "{uri:?}");
    // A missing URI field is the end of a link, and short forms do nothing.
    t.feed(b"\x1b]8\x07\x1b]8;\x07\x1b]8;;\x07z");
    check(&t);
    // The table is bounded however many distinct links are made.
    for n in 0..(crate::links::MAX_LINKS + 50) {
        t.feed(format!("\x1b]8;;http://h/{n}\x07.\x1b]8;;\x07").as_bytes());
    }
    assert!(t.state.links.len() <= crate::links::MAX_LINKS);
    check(&t);
}

#[test]
fn the_new_sequences_never_break_the_screen() {
    let mut t = term();
    for seq in [
        &b"\x1b[?1;1004;2026;9999h"[..],
        b"\x1b[?1;1004;2026l",
        b"\x1b=\x1b>",
        b"\x1b]0;\x07\x1b]2;x\x07\x1b]7;\x07\x1b]8;\x07",
        b"\x1b]8;;;;;\x07\x1b]8;a:b:c;u;v;w\x07",
        b"\x1b[>c\x1b[=c\x1b[>1c\x1b[>0;1c",
    ] {
        t.feed(seq);
        check(&t);
    }
}
