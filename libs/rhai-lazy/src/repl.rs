//! The REPL state machine: feed it one line at a time, it says what to show.
//!
//! It owns no I/O, so the same logic serves the console, the desktop Terminal
//! (a pipe pair, no pty) and the tests. Multi-line input works by asking Rhai's
//! parser: when it reports the text as *incomplete* the REPL keeps the lines
//! and asks for another (`More`); a genuine syntax error is reported at once.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use rhai::{Dynamic, Engine, ParseError, ParseErrorType, Position, Scope, AST};

use crate::outcome::{classify, Outcome};

/// Prompt for a fresh statement.
pub const PROMPT: &str = "rhai> ";
/// Prompt while a block, call or string is still open.
pub const CONTINUE_PROMPT: &str = "  ... ";

/// History entries kept per session (oldest dropped first).
const HISTORY_MAX: usize = 1000;
/// Bytes one pending multi-line entry may reach before it is discarded.
const PENDING_MAX: usize = 1 << 20;

const HELP: &str = "\
Enter Rhai code; an unfinished block continues on the next line.
  :help       this text
  :history    the entries typed so far (Up/Down recall them; !N or !! re-runs)
  :cancel     drop an unfinished multi-line entry
  :reset      forget all variables and functions
  :quit       leave (also Ctrl-D or exit(n))";

/// What the front end should do after a line.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// The entry is unfinished: show [`CONTINUE_PROMPT`] and read more.
    More,
    /// Nothing to show; prompt again.
    Quiet,
    /// A value or message for standard output.
    Show(String),
    /// An error message for standard error (with position information).
    Error(String),
    /// Leave the REPL with this status.
    Exit(i32),
}

/// One interactive session: variables, functions and history persist.
pub struct Repl<'e> {
    engine: &'e Engine,
    scope: Scope<'static>,
    /// Functions defined so far (statements of earlier entries are not kept).
    functions: AST,
    pending: String,
    history: Vec<String>,
}

impl<'e> Repl<'e> {
    pub fn new(engine: &'e Engine) -> Self {
        Self {
            engine,
            scope: Scope::new(),
            functions: AST::empty(),
            pending: String::new(),
            history: Vec::new(),
        }
    }

    /// Whether an unfinished entry is waiting (selects the prompt).
    pub fn is_continuing(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The prompt to show before reading the next line.
    pub fn prompt(&self) -> &'static str {
        if self.is_continuing() {
            CONTINUE_PROMPT
        } else {
            PROMPT
        }
    }

    /// The entries typed so far, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Abandon an unfinished multi-line entry (Ctrl-C at the prompt).
    pub fn interrupt(&mut self) {
        self.pending.clear();
    }

    /// `!!` (the last entry) or `!N` (entry N of `:history`) as its text.
    fn recall(&self, word: &str) -> Option<Result<String, String>> {
        let index = match word.strip_prefix('!')? {
            "!" => self.history.len(),
            digits if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) => {
                digits.parse().unwrap_or(usize::MAX)
            }
            _ => return None,
        };
        Some(
            index
                .checked_sub(1)
                .and_then(|i| self.history.get(i))
                .cloned()
                .ok_or_else(|| format!("{word}: no such history entry")),
        )
    }

    /// Handle one input line (without its newline).
    pub fn feed(&mut self, line: &str) -> Step {
        let trimmed = line.trim();
        if trimmed == ":cancel" && self.is_continuing() {
            self.pending.clear();
            return Step::Quiet;
        }
        if !self.is_continuing() {
            match self.recall(trimmed) {
                Some(Ok(entry)) => return self.feed_entry(&entry),
                Some(Err(message)) => return Step::Error(message),
                None => {}
            }
        }
        self.feed_entry(line)
    }

    fn feed_entry(&mut self, line: &str) -> Step {
        let trimmed = line.trim();
        if !self.is_continuing() && trimmed.starts_with(':') {
            return self.command(trimmed);
        }
        if self.is_continuing() {
            self.pending.push('\n');
        } else if trimmed.is_empty() {
            return Step::Quiet;
        }
        self.pending.push_str(line);
        if self.pending.len() > PENDING_MAX {
            self.pending.clear();
            return Step::Error(format!(
                "entry is longer than {PENDING_MAX} bytes; discarded"
            ));
        }
        self.run_pending()
    }

    fn command(&mut self, command: &str) -> Step {
        match command {
            ":help" | ":h" | ":?" => Step::Show(HELP.to_string()),
            ":history" => Step::Show(
                self.history
                    .iter()
                    .enumerate()
                    .map(|(i, entry)| format!("{:>4}  {entry}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            ":reset" => {
                self.scope.clear();
                self.functions = AST::empty();
                Step::Quiet
            }
            ":quit" | ":exit" | ":q" => Step::Exit(0),
            other => Step::Error(format!("unknown command {other} (try :help)")),
        }
    }

    fn remember(&mut self, entry: &str) {
        if self.history.len() == HISTORY_MAX {
            self.history.remove(0);
        }
        self.history.push(entry.to_string());
    }

    fn run_pending(&mut self) -> Step {
        let source = core::mem::take(&mut self.pending);
        let ast = match self.engine.compile_with_scope(&self.scope, &source) {
            Ok(ast) => ast,
            Err(error) if is_incomplete(&error, &source) => {
                self.pending = source;
                return Step::More;
            }
            Err(error) => {
                self.remember(&source);
                return Step::Error(format!("error: {error}"));
            }
        };
        self.remember(&source);
        let program = self.functions.merge(&ast);
        let result = self
            .engine
            .eval_ast_with_scope::<Dynamic>(&mut self.scope, &program);
        self.functions += ast.clone_functions_only();
        match classify(result) {
            Outcome::Value(value) if value.is_unit() => Step::Quiet,
            Outcome::Value(value) => Step::Show(format!("{value:?}")),
            Outcome::Exit(code) => Step::Exit(code),
            Outcome::Failed(message) => Step::Error(format!("error: {message}")),
            Outcome::OutputClosed => Step::Exit(0),
        }
    }
}

/// Where the text ends, in Rhai's 1-based (line, character) coordinates.
fn end_position(text: &str) -> (usize, usize) {
    let line = text.matches('\n').count() + 1;
    let last = text.rsplit('\n').next().unwrap_or("");
    (line, last.chars().count() + 1)
}

/// Whether a parse error only means "the text stops too early".
///
/// Rhai has no "incomplete" error kind, so this reads its behaviour (pinned by
/// the tests, since a Rhai upgrade could change it): running off the end is
/// `UnexpectedEOF`, or `MissingToken` reported *at* the end of the text (a
/// missing `;` or closing bracket), or an unterminated back-tick string. A
/// `MissingToken` earlier in the text (`1 2`) is a real syntax error.
pub fn is_incomplete(error: &ParseError, text: &str) -> bool {
    match &*error.0 {
        ParseErrorType::UnexpectedEOF => true,
        ParseErrorType::BadInput(lex) => {
            // Only a back-tick string may span lines; an unterminated
            // `"..."` is an error. An odd number of back-ticks means one is
            // still open.
            matches!(lex, rhai::LexError::UnterminatedString) && text.matches('`').count() % 2 == 1
        }
        ParseErrorType::MissingToken(..) => at(error.1, end_position(text)),
        _ => false,
    }
}

fn at(position: Position, (line, column): (usize, usize)) -> bool {
    position.line() == Some(line) && position.position().is_none_or(|p| p == column)
}
