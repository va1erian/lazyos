#![forbid(unsafe_code)]

//! Line-incremental syntax highlighting with a pluggable lexer.
//!
//! The editor never lexes a file as a whole. It keeps a [`HighlightCache`] that
//! stores one line's tokens plus the state either side of it, and re-lexes only
//! the lines an edit can affect: it walks forward from the changed line and
//! stops as soon as the state it carries matches the state the next line was
//! previously lexed with (and that line's text is unchanged).
//!
//! The language rules live behind the [`Highlighter`] trait, so the editor can
//! host any language (or none):
//!
//! * [`PlainText`] emits no tokens and carries no state; it is the default for
//!   [`Editor::new`](crate::Editor::new).
//! * `RhaiHighlighter` is the hand-written Rhai lexer, behind the
//!   `rhai-syntax` feature.
//!
//! A highlighter is *line-incremental*: [`Highlighter::lex_line`] receives the
//! [`LineState`] carried from the previous line and returns the tokens for this
//! line plus the state to carry on. Two lines lex identically whenever they have
//! the same text and the same incoming state, which is what makes the cache
//! correct.
//!
//! Token positions are *char* offsets within a line, not bytes, matching the
//! editor's [`Buffer`](crate::buffer::Buffer).

mod cache;
mod token;

#[cfg(feature = "rhai-syntax")]
mod rhai;

pub use cache::{BRACKET_SCAN_LINES, HighlightCache};
pub use token::{Highlighter, LineState, PlainText, Token, TokenClass};

/// Re-exports the Rhai highlighter when the `rhai-syntax` feature is on.
#[cfg(feature = "rhai-syntax")]
pub use rhai::RhaiHighlighter;

#[cfg(test)]
mod tests;
