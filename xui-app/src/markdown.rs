//! How LazyOS apps show Markdown in xui's read-only view (`xui_markdown::MarkdownView`): the page
//! style (code in JetBrains Mono, which [`crate::font::register_docs`] registers) and a highlighter
//! that colours ```` ```rhai ```` code blocks with the code editor's Rhai lexer. The Docs app and the
//! Messenger Playground's doc pane both use them, so a script reads the same in either.

use xui_code_editor::{Highlighter as _, LineState, RhaiHighlighter, TokenClass};
use xui_markdown::{CodeClass, CodeSpan, Highlighter, MarkdownStyle};

/// The page style: the UI face for text, JetBrains Mono for code.
pub fn style() -> MarkdownStyle {
    MarkdownStyle {
        code_family: Some(crate::font::MONO_FAMILY.to_owned()),
        ..MarkdownStyle::default()
    }
}

/// Colours code blocks whose fence names Rhai (```` ```rhai ````); others stay plain.
pub struct RhaiCode;

impl Highlighter for RhaiCode {
    fn line(&self, lang: &str, line: &str, state: u32) -> (Vec<CodeSpan>, u32) {
        if !lang.eq_ignore_ascii_case("rhai") {
            return (Vec::new(), 0);
        }
        let (tokens, next) = RhaiHighlighter.lex_line(line, &LineState::from_raw(state.into()));
        let bytes = char_starts(line);
        let spans = tokens
            .iter()
            .filter_map(|token| {
                let class = class_of(token.class)?;
                let start = *bytes.get(token.start)?;
                let end = *bytes.get(token.end)?;
                (start < end).then_some(CodeSpan {
                    range: start..end,
                    class,
                })
            })
            .collect();
        (spans, narrow(next.as_raw()))
    }
}

/// The byte offset of every char of `line`, and its length last: a token's char offsets index it.
fn char_starts(line: &str) -> Vec<usize> {
    let mut starts: Vec<usize> = line.char_indices().map(|(at, _)| at).collect();
    starts.push(line.len());
    starts
}

/// The view's colour class for a token; identifiers, operators and punctuation stay plain text.
fn class_of(class: TokenClass) -> Option<CodeClass> {
    match class {
        TokenClass::Keyword => Some(CodeClass::Keyword),
        TokenClass::Function => Some(CodeClass::Function),
        TokenClass::String | TokenClass::Interpolation => Some(CodeClass::String),
        TokenClass::Comment | TokenClass::DocComment => Some(CodeClass::Comment),
        TokenClass::Number => Some(CodeClass::Number),
        TokenClass::Identifier | TokenClass::Operator | TokenClass::Punctuation => None,
    }
}

/// The lexer's carried state in the view's 32 bits. The Rhai lexer keeps its mode in the low four
/// bits and a nesting depth above them; a depth too deep for the rest saturates, keeping the mode.
fn narrow(raw: u64) -> u32 {
    u32::try_from(raw).unwrap_or(!0xF | (raw & 0xF) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(line: &str, state: u32) -> (Vec<(String, CodeClass)>, u32) {
        let (spans, next) = RhaiCode.line("rhai", line, state);
        let shown = spans
            .into_iter()
            .map(|s| (line[s.range].to_owned(), s.class))
            .collect();
        (shown, next)
    }

    #[test]
    fn rhai_is_coloured_by_byte_range_and_other_languages_are_not() {
        let (shown, _) = classes("let caf\u{e9} = sys::open(\"x\"); // note", 0);
        assert!(shown.contains(&("let".to_owned(), CodeClass::Keyword)));
        assert!(shown.contains(&("\"x\"".to_owned(), CodeClass::String)));
        assert!(shown.contains(&("// note".to_owned(), CodeClass::Comment)));
        assert!(shown
            .iter()
            .any(|(t, c)| t == "open" && *c == CodeClass::Function));
        assert_eq!(RhaiCode.line("sh", "let x = 1", 0), (Vec::new(), 0));
    }

    #[test]
    fn a_block_comment_carries_into_the_next_line() {
        let (_, open) = classes("x /* starts", 0);
        assert_ne!(open, 0);
        let (shown, closed) = classes("still */ let", open);
        assert_eq!(shown[0], ("still */".to_owned(), CodeClass::Comment));
        assert_eq!(closed, 0);
    }

    #[test]
    fn a_deep_state_saturates_keeping_its_mode() {
        assert_eq!(narrow(0x15), 0x15);
        assert_eq!(narrow(!0xF | 0x5), !0xF_u32 | 0x5);
    }
}
