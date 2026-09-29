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
//! editor's [`Buffer`].

use crate::buffer::Buffer;

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
fn is_bracket(c: char) -> bool {
    matches!(c, '(' | ')' | '[' | ']' | '{' | '}')
}

/// The closing bracket matching an opening one.
fn matching_close(open: char) -> char {
    match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => open,
    }
}

/// The opening bracket matching a closing one.
fn matching_open(close: char) -> char {
    match close {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        _ => close,
    }
}

/// One line's cached tokens and the states either side of it.
#[derive(Clone, Debug)]
struct LexedLine {
    /// The line's text when it was lexed, checked to decide whether it is
    /// still current.
    text: String,
    /// The state the line was lexed with.
    start: LineState,
    /// The state after the line.
    end: LineState,
    /// The line's tokens.
    tokens: Vec<Token>,
    /// A placeholder for a line an edit inserted; it never counts as current.
    stale: bool,
}

impl LexedLine {
    /// A placeholder for an inserted line, re-lexed on the next pass.
    fn stale() -> LexedLine {
        LexedLine {
            text: String::new(),
            start: LineState::default(),
            end: LineState::default(),
            tokens: Vec::new(),
            stale: true,
        }
    }

    /// The code brackets on this line, as `(char column, bracket)`.
    ///
    /// A bracket counts when it is not inside a string, comment or
    /// interpolation token. A plain-text cache stores no tokens, so every
    /// bracket counts; a Rhai cache hides the brackets inside strings and
    /// comments. This keeps bracket matching independent of the highlighter.
    fn brackets(&self) -> impl Iterator<Item = (usize, char)> + '_ {
        let mut skip = self
            .tokens
            .iter()
            .filter(|token| {
                matches!(
                    token.class,
                    TokenClass::String
                        | TokenClass::Comment
                        | TokenClass::DocComment
                        | TokenClass::Interpolation
                )
            })
            .peekable();
        self.text.chars().enumerate().filter_map(move |(col, c)| {
            while skip.peek().is_some_and(|token| token.end <= col) {
                skip.next();
            }
            let covered = skip
                .peek()
                .is_some_and(|token| token.start <= col && col < token.end);
            (!covered && is_bracket(c)).then_some((col, c))
        })
    }
}

/// A per-line highlight cache that re-lexes only the lines an edit affects.
///
/// Every line remembers the state it was lexed with and the state it produced.
/// When a line is re-lexed and its outgoing state matches what the next line
/// previously started with (and that line's text is unchanged), the rest of the
/// file is still valid and lexing stops. Typing in the middle of a large,
/// balanced file therefore costs one line.
///
/// The cache owns its [`Highlighter`]; replacing it re-lexes the whole buffer,
/// because the tokens the old highlighter produced no longer apply.
pub struct HighlightCache {
    highlighter: Box<dyn Highlighter>,
    lines: Vec<LexedLine>,
}

impl HighlightCache {
    /// Lexes the whole buffer with `highlighter`.
    pub fn new(buffer: &Buffer, highlighter: impl Highlighter + 'static) -> HighlightCache {
        HighlightCache::with_boxed(buffer, Box::new(highlighter))
    }

    /// Lexes the whole buffer with an already-boxed `highlighter`.
    pub fn with_boxed(buffer: &Buffer, highlighter: Box<dyn Highlighter>) -> HighlightCache {
        let mut cache = HighlightCache {
            highlighter,
            lines: Vec::new(),
        };
        cache.reset(buffer);
        cache
    }

    /// Replaces the highlighter and re-lexes the whole buffer.
    pub fn set_highlighter(&mut self, buffer: &Buffer, highlighter: impl Highlighter + 'static) {
        self.set_boxed_highlighter(buffer, Box::new(highlighter));
    }

    /// Replaces the highlighter with an already-boxed one and re-lexes the
    /// whole buffer.
    pub fn set_boxed_highlighter(&mut self, buffer: &Buffer, highlighter: Box<dyn Highlighter>) {
        self.highlighter = highlighter;
        self.reset(buffer);
    }

    /// Clears the cache and lexes the whole buffer again.
    pub fn reset(&mut self, buffer: &Buffer) {
        self.lines.clear();
        self.relex(buffer, 0, 0);
    }

    /// Re-lexes from `from_line` until the state settles, returning the number
    /// of lines actually lexed.
    ///
    /// `from_line..=through_line` is the span an edit changed; the editor gets
    /// it from [`Buffer::take_dirty`]. Every line in it is re-lexed, and the
    /// pass only stops on a matching cache entry after it: inside a multi-line
    /// replacement an old entry can line up with new text by chance, and
    /// stopping there would leave the rest of the replacement stale.
    pub fn relex(&mut self, buffer: &Buffer, from_line: usize, through_line: usize) -> usize {
        let count = buffer.line_count();
        let from = from_line.min(self.lines.len()).min(count);
        // Keep the cache aligned with the buffer's lines: an edit at `from`
        // that added or removed lines shifted everything after it. Without
        // this every line below an Enter would miss the cache and be re-lexed.
        //
        // Added lines get placeholders *at* `from`, so the edited line itself
        // is always re-lexed: were they placed after it, an inserted line
        // whose text equals the old line at `from` would match that entry and
        // stop the pass before ever reaching the placeholders.
        if from < self.lines.len() {
            let cached = self.lines.len();
            if count > cached {
                let added = count - cached;
                let at = from;
                self.lines
                    .splice(at..at, std::iter::repeat_with(LexedLine::stale).take(added));
            } else if count < cached {
                let removed = (cached - count).min(cached - (from + 1));
                self.lines.drain(from + 1..from + 1 + removed);
            }
        }
        let mut state = if from == 0 {
            LineState::default()
        } else {
            self.lines[from - 1].end
        };

        let mut relexed = 0;
        let mut line = from;
        while line < count {
            let text = buffer.line_string(line);
            if line > through_line
                && let Some(cached) = self.lines.get(line)
                && !cached.stale
                && cached.text == text
                && cached.start == state
            {
                break;
            }
            let (tokens, end) = self.highlighter.lex_line(&text, &state);
            let lexed = LexedLine {
                text,
                start: state,
                end,
                tokens,
                stale: false,
            };
            if line < self.lines.len() {
                self.lines[line] = lexed;
            } else {
                self.lines.push(lexed);
            }
            state = end;
            relexed += 1;
            line += 1;
        }
        self.lines.truncate(count);
        relexed
    }

    /// The tokens of `line`, or an empty slice when it is out of range.
    pub fn tokens(&self, line: usize) -> &[Token] {
        self.lines
            .get(line)
            .map_or(&[], |line| line.tokens.as_slice())
    }

    /// The number of cached lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The state after `line`, for tests that inspect incremental state.
    pub fn state_after(&self, line: usize) -> Option<&LineState> {
        self.lines.get(line).map(|line| &line.end)
    }

    /// The two bracket char offsets to highlight for the caret, or `None`.
    ///
    /// The caret may sit either just before or just after a bracket. Brackets
    /// inside strings and comments are not code brackets and are skipped, which
    /// the cache decides from the token classes of the active highlighter; a
    /// plain-text cache marks every bracket as code.
    ///
    /// The search walks outward from the caret's bracket through the cached
    /// lines and stops at its match, so its cost is the distance between the
    /// two brackets (capped at [`BRACKET_SCAN_LINES`]), not the file size. It
    /// runs on every paint, so it must not scan the whole buffer.
    ///
    /// The `current` index into `self.lines` is what identifies the line to
    /// `valid_brackets`; clippy's iterator rewrite would obscure that.
    #[allow(clippy::needless_range_loop)]
    pub fn bracket_pair(&self, buffer: &Buffer, caret: usize) -> Option<(usize, usize)> {
        let (offset, bracket) = bracket_at(buffer, caret)?;
        let line = buffer.line_of_char(offset);
        let col = offset - buffer.line_start(line);
        // Only a code bracket counts; one inside a string or comment does not.
        if !self.lines.get(line)?.brackets().any(|(at, _)| at == col) {
            return None;
        }

        let forward = matching_close(bracket) != bracket;
        let (same, other) = if forward {
            (bracket, matching_close(bracket))
        } else {
            (bracket, matching_open(bracket))
        };
        let mut depth = 0usize;
        let mut step = |c: char| {
            if c == same {
                depth += 1;
            } else if c == other {
                depth -= 1;
                return depth == 0;
            }
            false
        };

        let last = self.lines.len().min(line + BRACKET_SCAN_LINES);
        let first = line.saturating_sub(BRACKET_SCAN_LINES);
        if forward {
            for current in line..last {
                let found = self.lines[current]
                    .brackets()
                    .filter(|&(at, _)| current != line || at >= col)
                    .find(|&(_, c)| step(c));
                if let Some((at, _)) = found {
                    return Some((offset, buffer.line_start(current) + at));
                }
            }
        } else {
            for current in (first..=line).rev() {
                let brackets: Vec<_> = self.lines[current].brackets().collect();
                let found = brackets
                    .into_iter()
                    .rev()
                    .filter(|&(at, _)| current != line || at <= col)
                    .find(|&(_, c)| step(c));
                if let Some((at, _)) = found {
                    return Some((buffer.line_start(current) + at, offset));
                }
            }
        }
        None
    }
}

/// How many lines [`HighlightCache::bracket_pair`] searches in each direction
/// before giving up, so an unmatched bracket in a huge file stays cheap.
pub const BRACKET_SCAN_LINES: usize = 2_000;

/// The bracket at or immediately before `caret`.
fn bracket_at(buffer: &Buffer, caret: usize) -> Option<(usize, char)> {
    if let Some(c) = buffer.char_at(caret)
        && is_bracket(c)
    {
        return Some((caret, c));
    }
    if caret > 0
        && let Some(c) = buffer.char_at(caret - 1)
        && is_bracket(c)
    {
        return Some((caret - 1, c));
    }
    None
}

/// The hand-written, line-incremental Rhai lexer.
#[cfg(feature = "rhai-syntax")]
mod rhai {
    use super::*;

    /// The state the lexer carries from the end of one line to the start of the
    /// next.
    ///
    /// Two lines lex identically whenever they have the same text and the same
    /// incoming state, which is what makes incremental re-lexing possible. It
    /// is internal to [`RhaiHighlighter`]; the editor only sees the packed
    /// [`LineState`].
    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    struct LexState {
        mode: Mode,
    }

    /// What the lexer is in the middle of at a line boundary.
    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    enum Mode {
        /// Ordinary code.
        #[default]
        Code,
        /// Inside a `/* ... */` block comment; `level` is the nesting depth.
        BlockComment {
            /// The comment nesting depth.
            level: usize,
            /// Whether the comment opened with `/**`.
            doc: bool,
        },
        /// Inside a `"..."` string continued by a trailing backslash.
        DoubleString,
        /// Inside a `` `...` `` string.
        Backtick,
        /// Inside a `#"..."#` raw string; `hashes` is the number of leading `#`.
        RawString {
            /// The number of `#` in the terminator.
            hashes: usize,
        },
        /// Inside a `${ ... }` interpolation; `depth` is the brace depth.
        Interpolation {
            /// The brace nesting depth, at least one.
            depth: usize,
        },
    }

    // The packed tags for `Mode`, in the low three bits of a `LineState`.
    const TAG_CODE: u64 = 0;
    const TAG_BLOCK_COMMENT: u64 = 1;
    const TAG_DOUBLE_STRING: u64 = 2;
    const TAG_BACKTICK: u64 = 3;
    const TAG_RAW_STRING: u64 = 4;
    const TAG_INTERPOLATION: u64 = 5;

    impl LexState {
        /// Packs this state into the editor's opaque [`LineState`].
        ///
        /// Three bits hold the mode tag, one bit the block-comment `doc` flag,
        /// and the rest the level/depth/hashes count (bounded in practice by a
        /// file's nesting depth).
        fn to_line_state(&self) -> LineState {
            let (tag, flag, value) = match self.mode {
                Mode::Code => (TAG_CODE, false, 0),
                Mode::BlockComment { level, doc } => (TAG_BLOCK_COMMENT, doc, level as u64),
                Mode::DoubleString => (TAG_DOUBLE_STRING, false, 0),
                Mode::Backtick => (TAG_BACKTICK, false, 0),
                Mode::RawString { hashes } => (TAG_RAW_STRING, false, hashes as u64),
                Mode::Interpolation { depth } => (TAG_INTERPOLATION, false, depth as u64),
            };
            LineState::from_raw(tag | u64::from(flag) << 3 | value << 4)
        }

        /// The inverse of [`LexState::to_line_state`].
        fn from_line_state(state: LineState) -> LexState {
            let raw = state.as_raw();
            let tag = raw & 0b111;
            let flag = (raw >> 3) & 1 == 1;
            let value = (raw >> 4) as usize;
            let mode = match tag {
                TAG_BLOCK_COMMENT => Mode::BlockComment {
                    level: value,
                    doc: flag,
                },
                TAG_DOUBLE_STRING => Mode::DoubleString,
                TAG_BACKTICK => Mode::Backtick,
                TAG_RAW_STRING => Mode::RawString { hashes: value },
                TAG_INTERPOLATION => Mode::Interpolation { depth: value },
                _ => Mode::Code,
            };
            LexState { mode }
        }
    }

    /// The hand-written Rhai language rules.
    ///
    /// This is the [`Highlighter`] the editor uses when the `rhai-syntax`
    /// feature is on. It is a pure value, so it can be copied into every editor.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct RhaiHighlighter;

    impl Highlighter for RhaiHighlighter {
        fn lex_line(&self, line: &str, state: &LineState) -> (Vec<Token>, LineState) {
            let (tokens, end) = lex_line(line, LexState::from_line_state(*state));
            (tokens, end.to_line_state())
        }
    }

    /// Lexes one line (without its terminator) given the incoming `state`.
    ///
    /// Returns the line's tokens and the state to start the next line with.
    fn lex_line(line: &str, state: LexState) -> (Vec<Token>, LexState) {
        let chars: Vec<char> = line.chars().collect();
        let mut lexer = Lexer {
            chars: &chars,
            pos: 0,
            mode: state.mode,
            tokens: Vec::new(),
        };
        lexer.run();
        (lexer.tokens, LexState { mode: lexer.mode })
    }

    /// Whether `text` is a Rhai keyword or reserved word.
    fn is_keyword(text: &str) -> bool {
        matches!(
            text,
            "true"
                | "false"
                | "let"
                | "const"
                | "if"
                | "else"
                | "switch"
                | "do"
                | "while"
                | "until"
                | "loop"
                | "for"
                | "in"
                | "fn"
                | "private"
                | "continue"
                | "break"
                | "return"
                | "throw"
                | "try"
                | "catch"
                | "import"
                | "export"
                | "as"
                | "public"
                | "package"
                | "super"
                | "async"
                | "await"
                | "use"
                | "case"
                | "this"
                | "global"
                | "static"
                | "var"
        )
    }

    /// Whether `c` may start an identifier.
    fn is_id_start(c: char) -> bool {
        c == '_' || c.is_alphabetic()
    }

    /// Whether `c` may continue an identifier.
    fn is_id_continue(c: char) -> bool {
        c == '_' || c.is_alphanumeric()
    }

    /// Whether `c` may appear in a symbolic operator.
    fn is_symbol(c: char) -> bool {
        matches!(
            c,
            '!' | '$'
                | '%'
                | '&'
                | '*'
                | '+'
                | '-'
                | '.'
                | '/'
                | ':'
                | '<'
                | '='
                | '>'
                | '?'
                | '@'
                | '^'
                | '|'
                | '~'
                | '#'
        )
    }

    /// Whether `c` is punctuation rather than an operator.
    fn is_punctuation(c: char) -> bool {
        is_bracket(c) || c == ',' || c == ';'
    }

    /// The scanner. It walks one line's chars and appends [`Token`]s.
    struct Lexer<'a> {
        chars: &'a [char],
        pos: usize,
        mode: Mode,
        tokens: Vec<Token>,
    }

    impl Lexer<'_> {
        /// The number of chars on the line.
        fn len(&self) -> usize {
            self.chars.len()
        }

        /// The char at `index`, if any.
        fn at(&self, index: usize) -> Option<char> {
            self.chars.get(index).copied()
        }

        /// Pushes a token, dropping empty spans.
        fn push(&mut self, class: TokenClass, start: usize, end: usize) {
            if end > start {
                self.tokens.push(Token { class, start, end });
            }
        }

        /// Runs the scanner: first resume any carried-over mode, then lex code.
        fn run(&mut self) {
            match self.mode.clone() {
                Mode::Code => {}
                Mode::BlockComment { level, doc } => self.block_comment_body(0, 0, level, doc),
                Mode::DoubleString => self.double_string_body(0, 0),
                Mode::Backtick => self.backtick_body(0, 0),
                Mode::RawString { hashes } => self.raw_string_body(0, 0, hashes),
                Mode::Interpolation { depth } => self.interpolation_body(0, depth),
            }
            self.scan_code();
        }

        /// Lexes ordinary code from the current position to the end of the line.
        fn scan_code(&mut self) {
            while self.pos < self.len() {
                let c = self.chars[self.pos];
                if c.is_whitespace() {
                    self.pos += 1;
                    continue;
                }
                match c {
                    '/' if self.at(self.pos + 1) == Some('/') => self.line_comment(),
                    '/' if self.at(self.pos + 1) == Some('*') => self.block_comment_start(),
                    '"' => self.double_string_body(self.pos, self.pos + 1),
                    '`' => self.backtick_body(self.pos, self.pos + 1),
                    '#' if self.raw_string_opens() => self.raw_string_start(),
                    '\'' => self.char_literal(),
                    c if c.is_ascii_digit() => self.number(),
                    c if is_id_start(c) => self.identifier(),
                    _ => self.symbol(),
                }
            }
        }

        /// A `//` or `///` comment, to the end of the line.
        fn line_comment(&mut self) {
            let start = self.pos;
            let doc = self.at(start + 2) == Some('/') && self.at(start + 3) != Some('/');
            let class = if doc {
                TokenClass::DocComment
            } else {
                TokenClass::Comment
            };
            self.push(class, start, self.len());
            self.pos = self.len();
        }

        /// The start of a `/* ... */` comment.
        fn block_comment_start(&mut self) {
            let start = self.pos;
            let doc = self.at(start + 2) == Some('*') && self.at(start + 3) != Some('*');
            self.block_comment_body(start, start + 2, 1, doc);
        }

        /// Scans a block comment body from `i` at nesting `level`.
        ///
        /// `token_start` is where the token being built begins; it is `0` when
        /// the comment was already open at the start of the line.
        fn block_comment_body(
            &mut self,
            token_start: usize,
            mut i: usize,
            mut level: usize,
            doc: bool,
        ) {
            let class = if doc {
                TokenClass::DocComment
            } else {
                TokenClass::Comment
            };
            while i < self.len() {
                match (self.chars[i], self.at(i + 1)) {
                    ('/', Some('*')) => {
                        level += 1;
                        i += 2;
                    }
                    ('*', Some('/')) => {
                        level -= 1;
                        i += 2;
                        if level == 0 {
                            self.push(class, token_start, i);
                            self.mode = Mode::Code;
                            self.pos = i;
                            return;
                        }
                    }
                    _ => i += 1,
                }
            }
            self.push(class, token_start, self.len());
            self.mode = Mode::BlockComment { level, doc };
            self.pos = self.len();
        }

        /// Scans a `"..."` string body from `i`, where `token_start` is the
        /// opening quote or the start of a continuation line.
        fn double_string_body(&mut self, token_start: usize, mut i: usize) {
            let mut carry = false;
            while i < self.len() {
                match self.chars[i] {
                    '\\' => {
                        if i + 1 < self.len() {
                            i += 2;
                        } else {
                            // A backslash at the end of the line continues it.
                            carry = true;
                            i += 1;
                        }
                    }
                    '"' => {
                        i += 1;
                        self.push(TokenClass::String, token_start, i);
                        self.mode = Mode::Code;
                        self.pos = i;
                        return;
                    }
                    _ => i += 1,
                }
            }
            self.push(TokenClass::String, token_start, self.len());
            self.mode = if carry {
                Mode::DoubleString
            } else {
                Mode::Code
            };
            self.pos = self.len();
        }

        /// Scans a `'x'` character literal.
        fn char_literal(&mut self) {
            let start = self.pos;
            let mut i = start + 1;
            while i < self.len() {
                match self.chars[i] {
                    '\\' => i += 2,
                    '\'' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            let end = i.min(self.len());
            self.push(TokenClass::String, start, end);
            self.pos = end;
        }

        /// Whether the `#`s at the cursor are followed by the opening `"` of a
        /// raw string (a bare `##` is not one).
        fn raw_string_opens(&self) -> bool {
            let mut j = self.pos;
            while self.at(j) == Some('#') {
                j += 1;
            }
            self.at(j) == Some('"')
        }

        /// The start of a `#"..."#` raw string.
        fn raw_string_start(&mut self) {
            let start = self.pos;
            let mut j = start;
            while self.at(j) == Some('#') {
                j += 1;
            }
            // The caller checked `raw_string_opens`, so `j` is the quote and
            // `j - start` is the number of hashes.
            self.raw_string_body(start, j + 1, j - start);
        }

        /// Scans a raw string body from `i`, terminated by `"` plus `hashes` `#`.
        fn raw_string_body(&mut self, token_start: usize, mut i: usize, hashes: usize) {
            while i < self.len() {
                if self.chars[i] == '"' {
                    let end = i + 1 + hashes;
                    if end <= self.len() && self.chars[i + 1..end].iter().all(|&c| c == '#') {
                        self.push(TokenClass::String, token_start, end);
                        self.mode = Mode::Code;
                        self.pos = end;
                        return;
                    }
                }
                i += 1;
            }
            self.push(TokenClass::String, token_start, self.len());
            self.mode = Mode::RawString { hashes };
            self.pos = self.len();
        }

        /// Scans the text of a back-tick string from `i`.
        fn backtick_body(&mut self, token_start: usize, mut i: usize) {
            while i < self.len() {
                match self.chars[i] {
                    '\\' => i = (i + 2).min(self.len()),
                    '`' => {
                        // A doubled back-tick is a literal back-tick.
                        if self.at(i + 1) == Some('`') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        self.push(TokenClass::String, token_start, i);
                        self.mode = Mode::Code;
                        self.pos = i;
                        return;
                    }
                    '$' if self.at(i + 1) == Some('{') => {
                        self.push(TokenClass::String, token_start, i);
                        self.interpolation_body(i, 1);
                        return;
                    }
                    _ => i += 1,
                }
            }
            self.push(TokenClass::String, token_start, self.len());
            self.mode = Mode::Backtick;
            self.pos = self.len();
        }

        /// Scans a `${ ... }` interpolation from `start` at brace `depth`.
        ///
        /// The whole segment, including the `$`, braces and expression, is one
        /// [`TokenClass::Interpolation`] token. When the matching `}` closes,
        /// lexing resumes inside the enclosing back-tick string.
        fn interpolation_body(&mut self, start: usize, mut depth: usize) {
            let mut i = start;
            if self.at(i) == Some('$') {
                i += 1;
                if self.at(i) == Some('{') {
                    i += 1;
                }
            }
            while i < self.len() {
                match self.chars[i] {
                    '{' => {
                        depth += 1;
                        i += 1;
                    }
                    '}' => {
                        depth -= 1;
                        i += 1;
                        if depth == 0 {
                            self.push(TokenClass::Interpolation, start, i);
                            self.mode = Mode::Backtick;
                            self.pos = i;
                            self.backtick_body(i, i);
                            return;
                        }
                    }
                    '\\' => i = (i + 2).min(self.len()),
                    _ => i += 1,
                }
            }
            self.push(TokenClass::Interpolation, start, self.len());
            self.mode = Mode::Interpolation { depth };
            self.pos = self.len();
        }

        /// Scans a number literal.
        fn number(&mut self) {
            let start = self.pos;
            let mut i = start;
            if self.chars[i] == '0'
                && let Some(prefix) = self.at(i + 1)
                && let Some(valid) = radix_predicate(prefix)
            {
                i += 2;
                while i < self.len() && (self.chars[i] == '_' || valid(self.chars[i])) {
                    i += 1;
                }
                self.push(TokenClass::Number, start, i);
                self.pos = i;
                return;
            }
            while i < self.len() && (self.chars[i].is_ascii_digit() || self.chars[i] == '_') {
                i += 1;
            }
            if self.at(i) == Some('.') && self.at(i + 1).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
                while i < self.len() && (self.chars[i].is_ascii_digit() || self.chars[i] == '_') {
                    i += 1;
                }
            }
            if matches!(self.at(i), Some('e' | 'E')) {
                let mut j = i + 1;
                if matches!(self.at(j), Some('+' | '-')) {
                    j += 1;
                }
                if self.at(j).is_some_and(|c| c.is_ascii_digit()) {
                    i = j;
                    while i < self.len() && (self.chars[i].is_ascii_digit() || self.chars[i] == '_')
                    {
                        i += 1;
                    }
                }
            }
            self.push(TokenClass::Number, start, i);
            self.pos = i;
        }

        /// Scans an identifier, keyword or function name.
        fn identifier(&mut self) {
            let start = self.pos;
            let mut i = start;
            while i < self.len() && is_id_continue(self.chars[i]) {
                i += 1;
            }
            let text: String = self.chars[start..i].iter().collect();
            let class = if is_keyword(&text) {
                TokenClass::Keyword
            } else if self.next_non_space_is(i, '(') {
                TokenClass::Function
            } else {
                TokenClass::Identifier
            };
            self.push(class, start, i);
            self.pos = i;
        }

        /// Whether the next non-whitespace char from `i` is `wanted`.
        fn next_non_space_is(&self, i: usize, wanted: char) -> bool {
            let mut j = i;
            while j < self.len() && self.chars[j].is_whitespace() {
                j += 1;
            }
            self.at(j) == Some(wanted)
        }

        /// Scans one punctuation char or a run of symbolic operator chars.
        fn symbol(&mut self) {
            let start = self.pos;
            if is_punctuation(self.chars[start]) {
                self.push(TokenClass::Punctuation, start, start + 1);
                self.pos = start + 1;
                return;
            }
            let mut i = start;
            while i < self.len() && is_symbol(self.chars[i]) {
                i += 1;
            }
            // Not every char routed here is symbolic (for example a stray `\` or
            // a Unicode mark). Consume one so the scanner always makes progress.
            if i == start {
                i += 1;
            }
            self.push(TokenClass::Operator, start, i);
            self.pos = i;
        }
    }

    /// The digit predicate for a `0x`/`0o`/`0b` radix prefix, or `None`.
    fn radix_predicate(prefix: char) -> Option<fn(char) -> bool> {
        match prefix {
            'x' | 'X' => Some(|c: char| c.is_ascii_hexdigit()),
            'o' | 'O' => Some(|c: char| ('0'..='7').contains(&c)),
            'b' | 'B' => Some(|c: char| c == '0' || c == '1'),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

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
        fn rhai_cache(buffer: &Buffer) -> HighlightCache {
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
            for ((raw_line, raw_tokens), (trait_line, trait_tokens, _)) in
                raw.iter().zip(via_trait.iter())
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

        // The oracle: Rhai's own tokenizer must start a token wherever this
        // lexer does, so no boundary is ever lost. Rhai merges a few constructs
        // (negative literals, `()` units) into one token, so this checks
        // containment rather than equality; the class tests above cover
        // classification.
        mod oracle {
            use super::*;
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
                        .filter(|token| {
                            matches!(token.class, TokenClass::Identifier | TokenClass::Function)
                        })
                        .map(|token| (token.start, token.start + token.len()))
                        .collect();
                    assert_eq!(ours, rhai_identifiers, "in {text:?}");
                }
            }
        }
    }
}

/// Re-exports the Rhai highlighter when the `rhai-syntax` feature is on.
#[cfg(feature = "rhai-syntax")]
pub use rhai::RhaiHighlighter;

#[cfg(test)]
mod tests {
    use super::*;

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
}
