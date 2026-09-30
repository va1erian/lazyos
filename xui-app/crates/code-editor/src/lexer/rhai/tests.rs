//! Tests for the Rhai scanner, its carried state and the incremental cache
//! over it.

use crate::buffer::Buffer;
use crate::lexer::{HighlightCache, Highlighter, LineState, PlainText, Token, TokenClass};

use super::RhaiHighlighter;
use super::scanner::lex_line;
use super::state::{LexState, Mode};

/// Lexes a whole string line by line with the raw lexer, returning
/// `(line index, tokens)`.
fn lex_all(text: &str) -> Vec<(usize, Vec<Token>)> {
    let mut state = LexState::default();
    let mut out = Vec::new();
    for (line, text) in text.split('\n').enumerate() {
        let (tokens, next) = lex_line(text, state);
        out.push((line, tokens));
        state = next;
    }
    out
}

/// The class of the tokens on `line` of `text`.
fn class_on(text: &str, line: usize) -> Vec<TokenClass> {
    lex_all(text)
        .into_iter()
        .find(|(index, _)| *index == line)
        .map(|(_, tokens)| tokens.into_iter().map(|token| token.class).collect())
        .unwrap_or_default()
}

/// Lexes a whole string through the [`Highlighter`] trait, line by line.
fn lex_all_via_trait(text: &str) -> Vec<(usize, Vec<Token>, LineState)> {
    let highlighter = RhaiHighlighter;
    let mut state = LineState::default();
    let mut out = Vec::new();
    for (line, text) in text.split('\n').enumerate() {
        let (tokens, next) = highlighter.lex_line(text, &state);
        out.push((line, tokens, next));
        state = next;
    }
    out
}

#[test]
fn keywords_and_identifiers_are_distinguished() {
    let tokens = class_on("let total = value;", 0);
    assert_eq!(tokens[0], TokenClass::Keyword);
    assert_eq!(tokens[1], TokenClass::Identifier);
}

#[test]
fn an_identifier_before_a_paren_is_a_function() {
    let tokens = class_on("fn add(a, b) { add(a, b) }", 0);
    assert_eq!(tokens[0], TokenClass::Keyword, "fn");
    assert_eq!(tokens[1], TokenClass::Function, "add");
    // The last `add` before `(` is a call.
    assert!(
        tokens
            .iter()
            .filter(|c| **c == TokenClass::Function)
            .count()
            >= 2
    );
}

#[test]
fn numbers_cover_radix_and_float_forms() {
    for text in ["42", "0xFF", "0b1010", "0o17", "1_000", "1.5", "1.5e-3"] {
        let (tokens, _) = lex_line(text, LexState::default());
        assert_eq!(tokens.len(), 1, "{text}");
        assert_eq!(tokens[0].class, TokenClass::Number, "{text}");
        assert_eq!(tokens[0].start, 0);
        assert_eq!(tokens[0].end, text.chars().count(), "{text}");
    }
}

#[test]
fn strings_comments_and_operators_are_tokenised() {
    let tokens = class_on(r#"let s = "hi" + 1; /* c */ // d"#, 0);
    assert!(tokens.contains(&TokenClass::String));
    assert!(tokens.contains(&TokenClass::Comment));
    assert!(tokens.contains(&TokenClass::Operator));
    assert!(tokens.contains(&TokenClass::Punctuation));
}

#[test]
fn unexpected_characters_are_tokenised_without_hanging() {
    // A stray backslash and a Unicode mark are neither identifiers nor
    // symbols; the scanner must still consume them.
    let text = "a \\ \u{2026} b";
    let (tokens, state) = lex_line(text, LexState::default());
    assert_eq!(state, LexState::default());
    assert!(tokens.iter().all(|token| token.end > token.start));
    assert_eq!(
        tokens.last().map(|token| token.end),
        Some(text.chars().count())
    );
}

#[test]
fn doc_comments_are_their_own_class() {
    assert_eq!(class_on("/// doc", 0)[0], TokenClass::DocComment);
    assert_eq!(class_on("// normal", 0)[0], TokenClass::Comment);
    assert_eq!(class_on("/** block doc */", 0)[0], TokenClass::DocComment);
    assert_eq!(class_on("/**** not doc */", 0)[0], TokenClass::Comment);
}

#[test]
fn a_block_comment_carries_its_state_across_lines() {
    let (_, state) = lex_line("let x = 1; /* open", LexState::default());
    assert!(matches!(state.mode, Mode::BlockComment { level: 1, .. }));
    let (tokens, state) = lex_line("still a comment", state);
    assert_eq!(tokens[0].class, TokenClass::Comment);
    assert!(matches!(state.mode, Mode::BlockComment { level: 1, .. }));
    let (tokens, state) = lex_line("end */ let y = 2;", state);
    assert_eq!(tokens[0].class, TokenClass::Comment);
    assert_eq!(state, LexState::default());
}

#[test]
fn nested_block_comments_are_counted() {
    let (_, state) = lex_line("/* a /* b */ c", LexState::default());
    assert!(matches!(state.mode, Mode::BlockComment { level: 1, .. }));
    let (_, state) = lex_line("*/ done", state);
    assert_eq!(state, LexState::default());
}

#[test]
fn a_backtick_string_carries_and_interpolates() {
    let (tokens, state) = lex_line("let s = `hello ${name} bye`;", LexState::default());
    let classes: Vec<_> = tokens.iter().map(|t| t.class).collect();
    assert!(classes.contains(&TokenClass::String));
    assert!(classes.contains(&TokenClass::Interpolation));
    assert_eq!(state, LexState::default());
}

#[test]
fn a_multiline_backtick_string_carries_its_state() {
    let (tokens, state) = lex_line("let s = `first", LexState::default());
    assert!(tokens.iter().any(|t| t.class == TokenClass::String));
    assert!(matches!(state.mode, Mode::Backtick));
    let (tokens, state) = lex_line("second`;", state);
    assert_eq!(tokens[0].class, TokenClass::String);
    assert_eq!(state, LexState::default());
}

#[test]
fn a_raw_string_carries_its_state() {
    let (tokens, state) = lex_line("let s = #\"first", LexState::default());
    assert!(tokens.iter().any(|t| t.class == TokenClass::String));
    assert!(matches!(state.mode, Mode::RawString { hashes: 1 }));
    let (tokens, state) = lex_line("second\"#;", state);
    assert_eq!(tokens[0].class, TokenClass::String);
    assert_eq!(state, LexState::default());
}

#[test]
fn hashes_without_a_quote_are_not_a_raw_string() {
    let (tokens, state) = lex_line("a ##", LexState::default());
    assert!(tokens.iter().all(|t| t.class != TokenClass::String));
    assert_eq!(state, LexState::default());
}

#[test]
fn a_double_quoted_string_continues_on_a_trailing_backslash() {
    let (_, state) = lex_line("let s = \"first\\", LexState::default());
    assert!(matches!(state.mode, Mode::DoubleString));
    let (tokens, state) = lex_line("second\";", state);
    assert_eq!(tokens[0].class, TokenClass::String);
    assert_eq!(state, LexState::default());
}

#[test]
fn this_and_other_reserved_words_are_keywords() {
    for word in ["this", "global", "static", "var"] {
        let (tokens, _) = lex_line(word, LexState::default());
        assert_eq!(tokens[0].class, TokenClass::Keyword, "{word}");
    }
}

/// Builds a cache over `buffer` with the Rhai highlighter.
pub(super) fn rhai_cache(buffer: &Buffer) -> HighlightCache {
    HighlightCache::new(buffer, RhaiHighlighter)
}

/// Relexes from the buffer's dirty line and checks the cache against a
/// fresh full lex, returning how many lines were lexed.
fn relex_and_check(cache: &mut HighlightCache, buffer: &mut Buffer) -> usize {
    let (from, through) = buffer.take_dirty().map_or((0, 0), |range| {
        (
            buffer.line_of_char(range.start),
            buffer.line_of_char(range.end),
        )
    });
    let relexed = cache.relex(buffer, from, through);
    let fresh = rhai_cache(buffer);
    assert_eq!(cache.line_count(), fresh.line_count());
    for line in 0..fresh.line_count() {
        assert_eq!(cache.tokens(line), fresh.tokens(line), "line {line}");
    }
    relexed
}

#[test]
fn line_cache_relexes_only_until_the_state_settles() {
    let mut text = String::new();
    for line in 0..1_000 {
        text.push_str(&format!("let x{line} = {line};\n"));
    }
    let mut buffer = Buffer::new(&text);
    let mut cache = rhai_cache(&buffer);
    assert_eq!(cache.line_count(), buffer.line_count());

    // Edit one line in the middle: only that line needs re-lexing.
    let start = buffer.line_start(500);
    buffer.insert(start, "// ", true);
    let dirty = buffer.take_dirty().expect("the edit is dirty");
    let from = buffer.line_of_char(dirty.start);
    let through = buffer.line_of_char(dirty.end);
    assert_eq!((from, through), (500, 500));
    assert_eq!(cache.relex(&buffer, from, through), 1);
}

#[test]
fn a_multi_line_replacement_is_lexed_in_full() {
    // The new `let` lines up with the old cached `let`, but the comment
    // the replacement opens must still reach the line after it.
    let mut buffer = Buffer::new("let\n1\n2\n");
    let mut cache = rhai_cache(&buffer);
    buffer.replace(0..6, "x\nlet\n/*\n", false);
    relex_and_check(&mut cache, &mut buffer);
    assert_eq!(cache.tokens(3)[0].class, TokenClass::Comment);
}

#[test]
fn undo_and_redo_mark_the_whole_change_dirty() {
    let mut buffer = Buffer::new("let\n1\n2\n");
    let mut cache = rhai_cache(&buffer);
    buffer.replace(0..6, "x\nlet\n/*\n", false);
    relex_and_check(&mut cache, &mut buffer);
    buffer.undo();
    relex_and_check(&mut cache, &mut buffer);
    buffer.redo();
    relex_and_check(&mut cache, &mut buffer);
}

#[test]
fn adding_or_removing_lines_does_not_relex_the_rest_of_the_file() {
    let mut text = String::new();
    for line in 0..1_000 {
        text.push_str(&format!("let x{line} = {line};\n"));
    }
    let mut buffer = Buffer::new(&text);
    let mut cache = rhai_cache(&buffer);

    // Enter in the middle of line 500 splits it in two.
    let middle = buffer.line_start(500) + 4;
    buffer.insert(middle, "\n", false);
    assert!(relex_and_check(&mut cache, &mut buffer) <= 2);

    // Pasting three lines at line 200.
    let at = buffer.line_start(200);
    buffer.insert(at, "let a = 1;\nlet b = 2;\nlet c = 3;\n", false);
    assert!(relex_and_check(&mut cache, &mut buffer) <= 4);

    // Duplicating line 300: the inserted text equals the line it goes
    // before, which must not stop the pass before the shifted line.
    let at = buffer.line_start(300);
    let line = format!("{}\n", buffer.line_string(300));
    buffer.insert(at, &line, false);
    assert!(relex_and_check(&mut cache, &mut buffer) <= 3);

    // Deleting two whole lines at line 100.
    let start = buffer.line_start(100);
    let end = buffer.line_start(102);
    buffer.remove(start..end, false);
    assert!(relex_and_check(&mut cache, &mut buffer) <= 2);
}

#[test]
fn opening_a_comment_relexes_until_the_close() {
    let text = "let a = 1;\nlet b = 2;\nlet c = 3;\nlet d = 4;\n";
    let mut buffer = Buffer::new(text);
    let mut cache = rhai_cache(&buffer);

    // Turn line 1 into an unterminated block comment: this propagates
    // to EOF.
    let start = buffer.line_start(1);
    buffer.insert(start, "/*", true);
    assert_eq!(cache.relex(&buffer, 1, 1), 4);

    // Close it on the first line again. The lines the previous edit
    // marked as comment are repaired, then the state is back to code.
    buffer.insert(buffer.line_start(1) + 2, "*/", true);
    assert_eq!(cache.relex(&buffer, 1, 1), 4);

    // With the cache consistent, a balanced edit settles after one line.
    buffer.insert(buffer.line_start(2), " ", true);
    assert_eq!(cache.relex(&buffer, 2, 2), 1);
}

#[test]
fn bracket_matching_finds_the_pair() {
    let text = "call(a + [b, c])";
    let buffer = Buffer::new(text);
    let cache = rhai_cache(&buffer);
    // The caret just after `(`.
    let open = text.find('(').expect("open paren");
    let close = text.rfind(')').expect("close paren");
    assert_eq!(cache.bracket_pair(&buffer, open + 1), Some((open, close)));
    // The caret just before `)`.
    assert_eq!(cache.bracket_pair(&buffer, close), Some((open, close)));
}

#[test]
fn bracket_matching_ignores_brackets_in_strings() {
    let text = "f(\")\");";
    let buffer = Buffer::new(text);
    let cache = rhai_cache(&buffer);
    let open = text.find('(').expect("open paren");
    let close = text.rfind(')').expect("close paren");
    assert_eq!(cache.bracket_pair(&buffer, open + 1), Some((open, close)));
}

#[test]
fn bracket_matching_spans_lines_and_nesting_in_both_directions() {
    let text = "fn f() {\n    if x { g(\"}\"); }\n    // }\n}\n";
    let buffer = Buffer::new(text);
    let cache = rhai_cache(&buffer);
    let open = text.find('{').expect("the function's brace");
    let close = text.rfind('}').expect("the closing brace");
    assert_eq!(cache.bracket_pair(&buffer, open), Some((open, close)));
    assert_eq!(cache.bracket_pair(&buffer, close), Some((open, close)));
}

#[test]
fn the_trait_wrapper_matches_the_raw_lexer_line_by_line() {
    // A block comment that opens on one line and closes on another, and
    // a back-tick string with `${}` interpolation, are the two ways the
    // carried state matters.
    let sample =
        "let s = \"hello\";\n/* block\n   comment */ let x = 1;\nlet t = `x ${1 + 2} y`;\n";
    let raw = lex_all(sample);
    let via_trait = lex_all_via_trait(sample);

    assert_eq!(raw.len(), via_trait.len());
    for ((raw_line, raw_tokens), (trait_line, trait_tokens, _)) in raw.iter().zip(via_trait.iter())
    {
        assert_eq!(raw_line, trait_line);
        assert_eq!(raw_tokens, trait_tokens, "line {raw_line}");
    }

    // The expected pre-refactor output, captured literally: the block
    // comment opens on line 1 and resumes on line 2, and the
    // interpolation is one token on line 3.
    assert_eq!(
        via_trait[1].1,
        vec![Token {
            class: TokenClass::Comment,
            start: 0,
            end: 8,
        }]
    );
    assert_eq!(
        via_trait[2].1,
        vec![
            Token {
                class: TokenClass::Comment,
                start: 0,
                end: 13,
            },
            Token {
                class: TokenClass::Keyword,
                start: 14,
                end: 17,
            },
            Token {
                class: TokenClass::Identifier,
                start: 18,
                end: 19,
            },
            Token {
                class: TokenClass::Operator,
                start: 20,
                end: 21,
            },
            Token {
                class: TokenClass::Number,
                start: 22,
                end: 23,
            },
            Token {
                class: TokenClass::Punctuation,
                start: 23,
                end: 24,
            },
        ]
    );
    assert_eq!(
        via_trait[3].1,
        vec![
            Token {
                class: TokenClass::Keyword,
                start: 0,
                end: 3,
            },
            Token {
                class: TokenClass::Identifier,
                start: 4,
                end: 5,
            },
            Token {
                class: TokenClass::Operator,
                start: 6,
                end: 7,
            },
            Token {
                class: TokenClass::String,
                start: 8,
                end: 11,
            },
            Token {
                class: TokenClass::Interpolation,
                start: 11,
                end: 19,
            },
            Token {
                class: TokenClass::String,
                start: 19,
                end: 22,
            },
            Token {
                class: TokenClass::Punctuation,
                start: 22,
                end: 23,
            },
        ]
    );
}

#[test]
fn a_highlighter_switch_relexes_the_whole_buffer() {
    let buffer = Buffer::new("let x = 1;\nlet y = 2;\n");
    let mut cache = HighlightCache::with_boxed(&buffer, Box::new(PlainText));
    assert!(cache.tokens(0).is_empty(), "plain text has no tokens");

    cache.set_highlighter(&buffer, RhaiHighlighter);
    // Every line was re-lexed with the new highlighter, not just the
    // first.
    assert!(!cache.tokens(0).is_empty());
    assert!(!cache.tokens(1).is_empty());
    assert_eq!(cache.tokens(0)[0].class, TokenClass::Keyword);
}
