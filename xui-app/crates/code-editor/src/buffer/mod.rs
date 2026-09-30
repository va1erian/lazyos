#![forbid(unsafe_code)]

//! The editor's text buffer: a [`ropey::Rope`], a line index and a coalescing
//! undo/redo stack.
//!
//! The buffer is deliberately free of UI code. Every operation works in *char*
//! indices (not bytes), so a multi-byte character is one caret step, and the
//! whole module is unit-tested without a backend.
//!
//! The line index is rebuilt lazily after an edit and cached, so a burst of
//! edits (a typing run) pays for it once, on the next read, rather than on every
//! keystroke. Reads still take `&self` because the index sits behind a
//! `RefCell`.
//!
//! Undo is stored as a stack of *groups*; a group is a list of splices applied
//! as one user action. Typing runs coalesce into a single group (the
//! [`Buffer::insert`] `coalesce` flag), while an explicit
//! [`Buffer::begin_edit`]/[`Buffer::end_edit`] pair forces one group for a
//! compound change such as indenting a block of lines.

mod history;
mod index;

#[cfg(test)]
mod tests;

use std::cell::{RefCell, RefMut};
use std::ops::Range;

use ropey::Rope;

use index::{LineIndex, line_content_len};

/// One reversible splice: `before` at `anchor` was replaced by `after`.
///
/// Applying it forward removes `before` and inserts `after`; applying it
/// backward does the reverse. All positions are char indices.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Edit {
    anchor: usize,
    before: String,
    after: String,
}

impl Edit {
    /// The length of `before` in chars.
    fn before_len(&self) -> usize {
        self.before.chars().count()
    }

    /// The length of `after` in chars.
    fn after_len(&self) -> usize {
        self.after.chars().count()
    }

    /// Whether this edit is a pure insertion (nothing removed).
    fn is_insertion(&self) -> bool {
        self.before.is_empty()
    }

    /// Whether this edit is a pure deletion (nothing inserted).
    fn is_deletion(&self) -> bool {
        self.after.is_empty()
    }

    /// Tries to fold `next` into this edit, returning whether it did. Only
    /// adjacent pure insertions or pure deletions coalesce, which is exactly
    /// what a run of typing or a run of backspaces produces.
    fn try_merge(&mut self, next: &Edit) -> bool {
        if self.is_insertion()
            && next.is_insertion()
            && next.anchor == self.anchor + self.after_len()
        {
            self.after.push_str(&next.after);
            return true;
        }
        if self.is_deletion() && next.is_deletion() {
            // Backspace: the new removal sits immediately before this one.
            if next.anchor + next.before_len() == self.anchor {
                let mut merged = next.before.clone();
                merged.push_str(&self.before);
                self.before = merged;
                self.anchor = next.anchor;
                return true;
            }
            // Forward delete: the new removal sits at the same anchor, since
            // the following chars shifted left after the first removal.
            if next.anchor == self.anchor {
                self.before.push_str(&next.before);
                return true;
            }
        }
        false
    }
}

/// A list of edits applied as one undoable action.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Group {
    edits: Vec<Edit>,
    /// Whether a further adjacent insertion/deletion may coalesce into this
    /// group. Set for typing runs, off for everything else.
    coalesce: bool,
}

/// The text buffer.
pub struct Buffer {
    rope: Rope,
    index: RefCell<LineIndex>,
    undo: Vec<Group>,
    redo: Vec<Group>,
    /// The group an explicit [`Buffer::begin_edit`] is accumulating into.
    pending: Option<Group>,
    /// The char range, in the current text, that edits have changed since the
    /// last [`Buffer::take_dirty`], used to re-lex only the affected lines.
    dirty: Option<Range<usize>>,
    /// Counts every text change, so a caller can tell whether a command
    /// changed the text without comparing it.
    revision: u64,
}

impl Buffer {
    /// A buffer holding `text`.
    pub fn new(text: &str) -> Buffer {
        let rope = Rope::from_str(text);
        let mut index = LineIndex::default();
        index.rebuild(&rope);
        Buffer {
            rope,
            index: RefCell::new(index),
            undo: Vec::new(),
            redo: Vec::new(),
            pending: None,
            dirty: None,
            revision: 0,
        }
    }

    /// A counter that moves whenever the text changes (an edit, an undo or a
    /// redo) and stays put otherwise.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether [`Buffer::undo`] has something to undo.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether [`Buffer::redo`] has something to redo.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Widens the dirty range for a change at `start` that replaced `removed`
    /// chars with `inserted` chars, keeping it in current-text coordinates.
    fn note_change(&mut self, start: usize, removed: usize, inserted: usize) {
        if removed == 0 && inserted == 0 {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        let new_end = start + inserted;
        self.dirty = Some(match self.dirty.take() {
            None => start..new_end,
            Some(range) => {
                // Move the old end by this change: past it, it shifts; inside
                // the removed text, it collapses to the new end; before the
                // change, it stays.
                let old_end = if range.end >= start + removed {
                    range.end - removed + inserted
                } else if range.end > start {
                    new_end
                } else {
                    range.end
                };
                range.start.min(start)..old_end.max(new_end)
            }
        });
    }

    /// Takes the char range edits have changed since the last call, in the
    /// current text, clearing it.
    ///
    /// The editor re-lexes from the range's first line and never stops before
    /// its last one, so a change at the end of a file only touches the last
    /// line and a multi-line replacement is always lexed in full.
    pub fn take_dirty(&mut self) -> Option<Range<usize>> {
        self.dirty.take()
    }

    /// The whole text.
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// The number of chars.
    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    /// The line index, rebuilt first if an edit made it stale.
    fn index(&self) -> RefMut<'_, LineIndex> {
        let mut index = self.index.borrow_mut();
        index.ensure(&self.rope);
        index
    }

    /// Marks the cached line index stale after an edit.
    fn invalidate_index(&self) {
        self.index.borrow_mut().stale = true;
    }

    /// The number of lines. A trailing newline yields a final empty line, as a
    /// text editor shows one.
    pub fn line_count(&self) -> usize {
        self.index().starts.len().max(1)
    }

    /// The number of chars in the longest line, for the horizontal extent.
    pub fn max_line_chars(&self) -> usize {
        self.index().max_line_chars
    }

    /// The widest line in display columns, with tabs expanded to
    /// `tab_width`. This is the horizontal extent the view scrolls over; it is
    /// cached until the next edit or a different tab width.
    pub fn max_line_cols(&self, tab_width: usize) -> usize {
        let mut index = self.index();
        if let Some((cached_tab, width)) = index.max_line_cols
            && cached_tab == tab_width
        {
            return width;
        }
        let width = (0..index.starts.len())
            .map(|line| {
                let slice = self.rope.line(line);
                slice
                    .chars()
                    .take(line_content_len(&slice))
                    .fold(0, |col, character| {
                        crate::text::advance(col, character, tab_width)
                    })
            })
            .max()
            .unwrap_or(0);
        index.max_line_cols = Some((tab_width, width));
        width
    }

    /// The zero-based line holding `char_idx`.
    pub fn line_of_char(&self, char_idx: usize) -> usize {
        self.index().line_of(char_idx.min(self.rope.len_chars()))
    }

    /// The char index where `line` starts.
    pub fn line_start(&self, line: usize) -> usize {
        self.index()
            .starts
            .get(line)
            .copied()
            .unwrap_or(self.rope.len_chars())
    }

    /// The char index just past `line`'s content, before any terminator.
    pub fn line_end(&self, line: usize) -> usize {
        let start = self.line_start(line);
        let stop = if line + 1 < self.line_count() {
            self.line_start(line + 1)
        } else {
            self.rope.len_chars()
        };
        let slice = self.rope.slice(start..stop);
        start + line_content_len(&slice)
    }

    /// `line`'s text without its line terminator.
    pub fn line_string(&self, line: usize) -> String {
        let start = self.line_start(line);
        let end = self.line_end(line);
        self.rope.slice(start..end).to_string()
    }

    /// The char at `char_idx`.
    pub fn char_at(&self, char_idx: usize) -> Option<char> {
        self.rope.get_char(char_idx)
    }

    /// The text in `range`, clamped to the buffer.
    pub fn slice(&self, range: Range<usize>) -> String {
        let start = range.start.min(self.rope.len_chars());
        let end = range.end.min(self.rope.len_chars()).max(start);
        self.rope.slice(start..end).to_string()
    }

    /// Inserts `text` at `at`. When `coalesce` is set and the previous edit was
    /// an adjacent typing run, the two share one undo group.
    pub fn insert(&mut self, at: usize, text: &str, coalesce: bool) {
        // An empty insert changes nothing, so it must not leave an undo step
        // (or a revision bump) behind.
        if text.is_empty() {
            return;
        }
        let at = at.min(self.rope.len_chars());
        self.note_change(at, 0, text.chars().count());
        let edit = Edit {
            anchor: at,
            before: String::new(),
            after: text.to_string(),
        };
        self.rope.insert(at, text);
        self.record(edit, coalesce);
    }

    /// Removes `range`.
    pub fn remove(&mut self, range: Range<usize>, coalesce: bool) {
        let start = range.start.min(self.rope.len_chars());
        let end = range.end.min(self.rope.len_chars()).max(start);
        if start == end {
            return;
        }
        self.note_change(start, end - start, 0);
        let before = self.rope.slice(start..end).to_string();
        let edit = Edit {
            anchor: start,
            before,
            after: String::new(),
        };
        self.rope.remove(start..end);
        self.record(edit, coalesce);
    }

    /// Replaces `range` with `text`.
    pub fn replace(&mut self, range: Range<usize>, text: &str, coalesce: bool) {
        let start = range.start.min(self.rope.len_chars());
        let end = range.end.min(self.rope.len_chars()).max(start);
        if start == end && text.is_empty() {
            return;
        }
        self.note_change(start, end - start, text.chars().count());
        let before = self.rope.slice(start..end).to_string();
        let edit = Edit {
            anchor: start,
            before,
            after: text.to_string(),
        };
        self.rope.remove(start..end);
        self.rope.insert(start, text);
        self.record(edit, coalesce);
    }
}
