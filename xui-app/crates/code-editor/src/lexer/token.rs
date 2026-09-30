//! The lexer's public vocabulary: token classes, highlighted spans, the opaque
//! carried state, the [`Highlighter`] trait and the no-op [`PlainText`], plus
//! the bracket helpers the cache and the Rhai punctuation rule share.

/// A lexical class, which the painter maps to a theme colour.
///
/// It is language-neutral: a highlighter chooses the classes that fit its
/// language and leaves the rest unused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenClass {
    /// A keyword such as `let`, `fn` or `if`.
    Keyword,
    /// An identifier.
    Identifier,
    /// A numeric literal, including hex/octal/binary and floats.
    Number,
    /// A string literal body, including its delimiters.
    String,
    /// An interpolated `${ ... }` segment inside a back-tick string.
    Interpolation,
    /// A `//` or `/* ... */` comment.
    Comment,
    /// A `///`, `//!` or `/** ... */` doc comment.
    DocComment,
    /// An operator such as `+`, `==`, `=>` or `..=`.
    Operator,
    /// Punctuation: brackets and the separators `,` and `;`.
    Punctuation,
    /// An identifier immediately followed by `(`, i.e. a function call or
    /// definition name.
    Function,
}

/// A highlighted span within one line.
///
/// `start` and `end` are char offsets and `end` is exclusive. Whitespace between
/// tokens is deliberately not covered, so a run of spaces is simply not painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    /// The lexical class.
    pub class: TokenClass,
    /// The first char offset.
    pub start: usize,
    /// The char offset just past the token.
    pub end: usize,
}

impl Token {
    /// The length of the token in chars.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether the token is empty, which should never be emitted.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// The opaque state a highlighter carries from one line to the next.
///
/// A highlighter packs whatever it needs (a block-comment depth, a string mode,
/// a bracket nesting level) into the raw `u64`; the editor only compares states
/// for equality and never inspects them. [`LineState::default`] is the state at
/// the start of a file and the state [`PlainText`] always returns.
///
/// The value is `Copy + Eq + Default + Debug`, which is all the incremental
/// cache needs to decide whether a re-lex has settled back into a state the rest
/// of the file was already lexed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct LineState(u64);

impl LineState {
    /// A state from a raw packed value, for highlighters with custom state.
    pub const fn from_raw(value: u64) -> LineState {
        LineState(value)
    }

    /// The raw packed value, the inverse of [`LineState::from_raw`].
    pub const fn as_raw(self) -> u64 {
        self.0
    }
}

/// A line-incremental lexer for one language.
///
/// The editor holds a `Box<dyn Highlighter>`, so an editor type is not generic
/// over its language. Implementors should keep their own state type out of the
/// trait and pack it into [`LineState`].
pub trait Highlighter {
    /// Lexes one line (without its terminator) given the incoming `state`.
    ///
    /// Returns the line's tokens and the state to start the next line with.
    fn lex_line(&self, line: &str, state: &LineState) -> (Vec<Token>, LineState);
}

/// The no-op highlighter: no tokens, no state.
///
/// This is the default for [`Editor::new`](crate::Editor::new): a plain-text
/// editor that still gets the buffer, view, editing, undo and clipboard, but no
/// colours. Bracket matching works normally because every bracket is code.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainText;

impl Highlighter for PlainText {
    fn lex_line(&self, _line: &str, state: &LineState) -> (Vec<Token>, LineState) {
        (Vec::new(), *state)
    }
}

/// Whether `c` is a bracket.
pub(super) fn is_bracket(c: char) -> bool {
    matches!(c, '(' | ')' | '[' | ']' | '{' | '}')
}

/// The closing bracket matching an opening one.
pub(super) fn matching_close(open: char) -> char {
    match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => open,
    }
}

/// The opening bracket matching a closing one.
pub(super) fn matching_open(close: char) -> char {
    match close {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        _ => close,
    }
}
