//! The per-line highlight cache: the [`HighlightCache`] that re-lexes only the
//! lines an edit affects, plus the bracket-pair lookup over it.

use crate::buffer::Buffer;

use super::token::{
    Highlighter, LineState, Token, TokenClass, is_bracket, matching_close, matching_open,
};

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
