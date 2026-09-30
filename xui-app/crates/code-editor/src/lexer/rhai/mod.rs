//! The hand-written, line-incremental Rhai lexer.
//!
//! This module owns the Rhai [`Highlighter`](crate::Highlighter): the carried
//! [`LexState`](state::LexState), its packed [`Mode`](state::Mode), the keyword
//! and character-class tables and the single-line scanner. It is compiled only
//! with the `rhai-syntax` feature, so a plain-text editor carries no language
//! rules.

mod keywords;
mod scanner;
mod state;

#[cfg(test)]
mod oracle;
#[cfg(test)]
mod tests;

use crate::lexer::{Highlighter, LineState, Token};

use state::LexState;

/// The hand-written Rhai language rules.
///
/// This is the [`Highlighter`] the editor uses when the `rhai-syntax`
/// feature is on. It is a pure value, so it can be copied into every editor.
#[derive(Clone, Copy, Debug, Default)]
pub struct RhaiHighlighter;

impl Highlighter for RhaiHighlighter {
    fn lex_line(&self, line: &str, state: &LineState) -> (Vec<Token>, LineState) {
        let (tokens, end) = scanner::lex_line(line, LexState::from_line_state(*state));
        (tokens, end.to_line_state())
    }
}
