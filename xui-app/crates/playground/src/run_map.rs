//! Putting a script and the prelude together, and mapping the positions the
//! child's Rhai errors report back to lines of the editor buffer.
//!
//! Rhai prints `(line N, position M)` at the end of an error, counting the
//! prelude's lines; a function called from a module adds `@ 'sys'` to a
//! position inside that module's source, which is not the user's text.

use crate::prelude;

/// A script ready to hand to the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    /// Prelude (when asked for) followed by the script.
    pub text: String,
    /// Lines the prelude adds in front of the script.
    pub offset: usize,
    /// The editor line the script's first line is on (1 for a whole
    /// buffer, the selection's first line for a selection).
    pub first_line: usize,
}

/// Prefix the prelude when `with_prelude` (or when the script mentions a
/// helper and `auto` is true). A leading `#!` line is blanked, as `rhai` does
/// for a script file, so every line number stays put.
pub fn compose(script: &str, with_prelude: bool, first_line: usize) -> Composed {
    let body = rhai_lazy::outcome::strip_shebang(script);
    if with_prelude {
        Composed {
            text: format!("{}{body}", prelude::SOURCE),
            offset: prelude::lines(),
            first_line: first_line.max(1),
        }
    } else {
        Composed {
            text: body.to_string(),
            offset: 0,
            first_line: first_line.max(1),
        }
    }
}

/// Like [`compose`], taking the prelude when the script uses a helper.
pub fn compose_auto(script: &str, first_line: usize) -> Composed {
    compose(script, prelude::wanted(script), first_line)
}

/// Where in the editor an error points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorMark {
    /// The editor line (1-based); `None` for a position inside the prelude.
    pub line: Option<usize>,
    pub column: usize,
    /// The line of output the mark came from.
    pub text: String,
}

impl ErrorMark {
    pub fn in_prelude(&self) -> bool {
        self.line.is_none()
    }
}

/// Every `(line N, position M)` in `text` that is not tagged with a module
/// source (`@ 'sys' (line ...)`).
fn positions(text: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut rest = text;
    let mut consumed = 0;
    while let Some(at) = rest.find("(line ") {
        let tail = &rest[at + "(line ".len()..];
        let before = &text[..consumed + at];
        if let Some((line, column, used)) = parse_position(tail) {
            if !tagged_with_module(before) {
                found.push((line, column));
            }
            consumed += at + "(line ".len() + used;
            rest = &text[consumed..];
        } else {
            consumed += at + "(line ".len();
            rest = &text[consumed..];
        }
    }
    found
}

/// `12, position 5)...` -> `(12, 5, bytes used)`.
fn parse_position(tail: &str) -> Option<(usize, usize, usize)> {
    let (line_text, rest) = tail.split_once(", position ")?;
    let line: usize = line_text.parse().ok()?;
    let close = rest.find(')')?;
    let column: usize = rest[..close].parse().ok()?;
    Some((
        line,
        column,
        line_text.len() + ", position ".len() + close + 1,
    ))
}

/// Whether the text before a position ends with `@ 'name' `.
fn tagged_with_module(before: &str) -> bool {
    let trimmed = before.trim_end();
    trimmed.ends_with('\'')
        && trimmed[..trimmed.len() - 1]
            .rsplit_once('\'')
            .is_some_and(|(head, _)| head.trim_end().ends_with('@'))
}

impl Composed {
    /// Map one line of the child's error output to an editor position: the
    /// last position in it that is the user's text. `None` when the line
    /// carries none (a Messenger error, a division by zero).
    pub fn map_error(&self, output: &str) -> Option<ErrorMark> {
        let (line, column) = *positions(output).last()?;
        Some(ErrorMark {
            line: line
                .checked_sub(self.offset)
                .filter(|l| *l >= 1)
                .map(|l| l + self.first_line - 1),
            column,
            text: output.to_string(),
        })
    }
}
