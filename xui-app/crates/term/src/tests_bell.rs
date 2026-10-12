//! The BEL counter and the helpers around search and links.

use crate::Term;

#[test]
fn bells_are_counted_and_taken() {
    let mut t = Term::new(20, 3);
    assert_eq!(t.take_bells(), 0);
    t.feed(b"a\x07b\x07\x07c");
    assert_eq!(t.take_bells(), 3);
    assert_eq!(t.take_bells(), 0);
    assert_eq!(t.row_text(0), "abc", "a bell draws nothing");
}

#[test]
fn a_bell_that_ends_an_osc_is_not_a_bell() {
    let mut t = Term::new(20, 3);
    t.feed(b"\x1b]0;title\x07\x1b]8;;http://a\x07x\x1b]8;;\x07");
    assert_eq!(t.take_bells(), 0);
    // But a bell inside a CSI sequence's neighbourhood still counts.
    t.feed(b"\x1b[1m\x07");
    assert_eq!(t.take_bells(), 1);
}

#[test]
fn a_flood_of_bells_cannot_overflow_the_counter() {
    let mut t = Term::new(20, 3);
    t.feed(&vec![7u8; 100_000]);
    assert_eq!(t.take_bells(), 100_000);
}
