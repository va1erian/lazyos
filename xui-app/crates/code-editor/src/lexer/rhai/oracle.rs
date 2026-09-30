//! Cross-checks the hand-written lexer against Rhai's own tokenizer, which acts
//! as the oracle for token boundaries and identifier spans.

use crate::buffer::Buffer;
use crate::lexer::TokenClass;

use super::scanner::lex_line;
use super::state::LexState;
use super::tests::rhai_cache;

use ::rhai::{Engine, Token};

/// Sample scripts the lexer and the oracle both understand.
/// Interpolated back-tick strings are excluded because Rhai's
/// tokenizer needs the parser's control block to resume them, which
/// a raw token stream does not have.
const SAMPLES: &[&str] = &[
    "let x = 42;\nlet y = 0xFF + 0b10 + 0o7;\n",
    "let s = \"hello\"; let t = 'c';\n",
    "fn add(a, b) { a + b }\nadd(1, 2);\n",
    "if x > 0 { print(x); } else { print(-x); }\n",
    "// line comment\n/// doc\n/* block */ let z = 0;\n",
    "for i in 0..10 { arr[i] += i; }\n",
    "let m = #{ key: \"value\", n: 1 };\n",
    "x?.foo(); x ?? 2; a && b || !c;\n",
    "let r = 1.5e-3 + 0.25;\n",
    "let raw = #\"a raw \" string\"#;\n",
    "while true { break; } loop { continue; }\n",
    "try { throw \"x\"; } catch (e) { }\n",
];

/// The absolute char offset of every token start this lexer
/// produces.
fn our_starts(text: &str) -> Vec<usize> {
    let buffer = Buffer::new(text);
    let cache = rhai_cache(&buffer);
    let mut starts = Vec::new();
    for line in 0..cache.line_count() {
        let base = buffer.line_start(line);
        for token in cache.tokens(line) {
            starts.push(base + token.start);
        }
    }
    starts
}

/// The absolute char offset of every token start Rhai's tokenizer
/// emits.
fn rhai_starts(text: &str) -> Vec<usize> {
    let engine = Engine::new();
    let (mut iterator, _control) = engine.lex([&text]);
    iterator.state.include_comments = true;

    let buffer = Buffer::new(text);
    let mut starts = Vec::new();
    for (token, position) in iterator {
        if matches!(token, Token::EOF) {
            break;
        }
        if let Some(line) = position.line() {
            let column = position.position().map_or(0, |column| column - 1);
            starts.push(buffer.line_start(line - 1) + column);
        }
    }
    starts
}

#[test]
fn our_token_boundaries_contain_rhais() {
    for sample in SAMPLES {
        let ours = our_starts(sample);
        for start in rhai_starts(sample) {
            assert!(
                ours.contains(&start),
                "Rhai token start {start} is missing from our lexer in:\n{sample}"
            );
        }
    }
}

#[test]
fn our_classes_agree_with_rhai_on_identifiers() {
    // Rhai only reports token starts, but an identifier's content
    // gives its exact length, so both ends can be compared,
    // including for a final identifier followed by trailing
    // whitespace.
    for text in ["let  total = add(a, b);", "let final_name   "] {
        let engine = Engine::new();
        let (iterator, _control) = engine.lex([&text]);
        let mut rhai_identifiers = Vec::new();
        for (token, position) in iterator {
            if matches!(token, Token::EOF) {
                break;
            }
            if let Token::Identifier(name) = &token
                && let Some(column) = position.position()
            {
                let start = column - 1;
                rhai_identifiers.push((start, start + name.chars().count()));
            }
        }

        let (tokens, _) = lex_line(text, LexState::default());
        let ours: Vec<(usize, usize)> = tokens
            .iter()
            .filter(|token| matches!(token.class, TokenClass::Identifier | TokenClass::Function))
            .map(|token| (token.start, token.start + token.len()))
            .collect();
        assert_eq!(ours, rhai_identifiers, "in {text:?}");
    }
}
