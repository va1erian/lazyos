//! Tests for the plain-text highlighter and the `HighlightCache` that wraps it.

use crate::buffer::Buffer;

use super::{HighlightCache, Highlighter, LineState, PlainText};

#[test]
fn plain_text_produces_no_tokens_and_keeps_the_state() {
    let highlighter = PlainText;
    let state = LineState::from_raw(7);
    let (tokens, end) = highlighter.lex_line("let x = 1;", &state);
    assert!(tokens.is_empty(), "plain text has no tokens");
    assert_eq!(end, state, "plain text carries the state through unchanged");
}

#[test]
fn editing_a_plain_text_buffer_keeps_the_cache_valid() {
    let mut buffer = Buffer::new("first\nsecond\nthird\n");
    let mut cache = HighlightCache::new(&buffer, PlainText);
    assert_eq!(cache.line_count(), buffer.line_count());

    buffer.insert(0, "zero\n", false);
    let dirty = buffer.take_dirty().expect("the edit is dirty");
    let from = buffer.line_of_char(dirty.start);
    let through = buffer.line_of_char(dirty.end);
    cache.relex(&buffer, from, through);

    assert_eq!(cache.line_count(), buffer.line_count());
    for line in 0..cache.line_count() {
        assert!(cache.tokens(line).is_empty(), "line {line}");
    }
}

#[test]
fn plain_text_counts_brackets_inside_what_rhai_calls_a_string() {
    // Rhai would hide the `)` inside the quotes; plain text must not.
    let text = "f(\")\");";
    let buffer = Buffer::new(text);
    let cache = HighlightCache::new(&buffer, PlainText);
    let open = text.find('(').expect("open paren");
    // The `)` inside the string closes the pair for plain text.
    let inside = text.find(')').expect("the string's paren");
    assert_eq!(cache.bracket_pair(&buffer, open + 1), Some((open, inside)));
}

#[test]
fn bracket_matching_needs_a_code_bracket() {
    // A caret on an identifier is not on a bracket at all.
    let buffer = Buffer::new("abc");
    let cache = HighlightCache::new(&buffer, PlainText);
    assert_eq!(cache.bracket_pair(&buffer, 1), None);
}

#[test]
fn an_empty_buffer_lexes_to_one_plain_line() {
    let buffer = Buffer::new("");
    let cache = HighlightCache::new(&buffer, PlainText);
    assert_eq!(cache.line_count(), buffer.line_count());
    assert!(cache.tokens(0).is_empty());
    assert_eq!(cache.state_after(0), Some(&LineState::default()));
}
