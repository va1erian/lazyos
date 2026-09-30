//! The Rhai scanner: a single-line state machine that resumes any carried-over
//! [`Mode`] and then lexes code, appending [`Token`]s.

use crate::lexer::{Token, TokenClass};

use super::keywords::{is_id_continue, is_id_start, is_keyword, is_punctuation, is_symbol};
use super::state::{LexState, Mode};

/// Lexes one line (without its terminator) given the incoming `state`.
///
/// Returns the line's tokens and the state to start the next line with.
pub(super) fn lex_line(line: &str, state: LexState) -> (Vec<Token>, LexState) {
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
                while i < self.len() && (self.chars[i].is_ascii_digit() || self.chars[i] == '_') {
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
