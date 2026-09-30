//! The Rhai lexer's carried state: the packed [`Mode`] and its conversion to
//! and from the editor's opaque [`LineState`].

use crate::lexer::LineState;

/// The state the lexer carries from the end of one line to the start of the
/// next.
///
/// Two lines lex identically whenever they have the same text and the same
/// incoming state, which is what makes incremental re-lexing possible. It
/// is internal to [`RhaiHighlighter`](super::RhaiHighlighter); the editor only
/// sees the packed [`LineState`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(super) struct LexState {
    pub(super) mode: Mode,
}

/// What the lexer is in the middle of at a line boundary.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(super) enum Mode {
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
    pub(super) fn to_line_state(&self) -> LineState {
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
    pub(super) fn from_line_state(state: LineState) -> LexState {
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
