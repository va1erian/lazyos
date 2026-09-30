#![forbid(unsafe_code)]

//! The editor's edit operations, independent of xui and of the event loop.
//!
//! Each function takes the buffer, the view and (where it needs it) the
//! clipboard, and returns whether the text changed so the widget can raise
//! `on_change`. Keeping the rules here means typing, auto-indent, indentation,
//! cut/copy/paste and undo are all unit-tested directly.

use crate::buffer::Buffer;
use crate::options::Options;
use crate::platform::Clipboard;
use crate::text::leading_whitespace;
use crate::view::View;

/// Replaces the selection (or inserts at the caret) with `text`.
pub(crate) fn splice(buffer: &mut Buffer, view: &mut View, text: &str, coalesce: bool) {
    match view.selection() {
        Some((start, end)) => {
            buffer.replace(start..end, text, coalesce);
            view.caret = start + text.chars().count();
        }
        None => {
            buffer.insert(view.caret, text, coalesce);
            view.caret += text.chars().count();
        }
    }
    view.anchor = view.caret;
    view.goal_col = None;
}

/// Inserts a typed character, coalescing with the previous typing run.
pub fn type_char(buffer: &mut Buffer, view: &mut View, character: char) {
    splice(buffer, view, &character.to_string(), true);
}

/// Inserts `text` as a run of typed characters.
#[cfg(test)]
pub fn type_text(buffer: &mut Buffer, view: &mut View, text: &str) {
    splice(buffer, view, text, true);
}

/// Replaces the selection (if any) with a newline that copies the indentation
/// of the line the selection starts on.
pub fn enter(buffer: &mut Buffer, view: &mut View) {
    let selection = view.selection();
    let at = selection.map_or(view.caret, |(start, _)| start);
    let line = buffer.line_of_char(at);
    let start = buffer.line_start(line);
    let column = at - start;
    let indent = {
        let text = buffer.line_string(line);
        let leading = leading_whitespace(&text);
        let limit = leading.chars().count().min(column);
        leading.chars().take(limit).collect::<String>()
    };
    let inserted = format!("\n{indent}");
    // An explicit edit so the newline and the indentation undo together, but
    // separately from the typing run before it.
    buffer.begin_edit();
    if let Some((start, end)) = selection {
        buffer.remove(start..end, false);
    }
    buffer.insert(at, &inserted, false);
    buffer.end_edit();
    view.caret = at + inserted.chars().count();
    view.anchor = view.caret;
    view.goal_col = None;
}

/// Deletes the selection, or the character before the caret.
pub fn backspace(buffer: &mut Buffer, view: &mut View) {
    if let Some((start, end)) = view.selection() {
        buffer.replace(start..end, "", false);
        view.caret = start;
    } else if view.caret > 0 {
        buffer.remove(view.caret - 1..view.caret, true);
        view.caret -= 1;
    }
    view.anchor = view.caret;
    view.goal_col = None;
}

/// Deletes the selection, or the character after the caret.
pub fn delete_forward(buffer: &mut Buffer, view: &mut View) {
    if let Some((start, end)) = view.selection() {
        buffer.replace(start..end, "", false);
        view.caret = start;
    } else if view.caret < buffer.len_chars() {
        buffer.remove(view.caret..view.caret + 1, true);
    }
    view.anchor = view.caret;
    view.goal_col = None;
}

/// The inclusive line range a selection touches.
fn selected_lines(buffer: &Buffer, view: &View) -> (usize, usize) {
    let (start, end) = view.selection().unwrap_or((view.caret, view.caret));
    let first = buffer.line_of_char(start);
    let mut last = buffer.line_of_char(end);
    // A selection that ends at the very start of a line does not include it.
    if last > first && buffer.line_start(last) == end {
        last -= 1;
    }
    (first, last)
}

/// Indents every line the selection touches, or inserts one indent at the
/// caret. Returns whether the text changed.
pub fn indent(buffer: &mut Buffer, view: &mut View, options: &Options) -> bool {
    if view.selection().is_none() {
        splice(buffer, view, &options.indent(), false);
        return !options.indent().is_empty();
    }
    let (first, last) = selected_lines(buffer, view);
    let unit = options.indent();
    let width = unit.chars().count();
    let mut starts: Vec<usize> = (first..=last).map(|line| buffer.line_start(line)).collect();
    starts.sort_unstable();
    buffer.begin_edit();
    for start in starts.into_iter().rev() {
        buffer.insert(start, &unit, false);
        shift(buffer, view, start, width as i64);
    }
    buffer.end_edit();
    view.goal_col = None;
    !unit.is_empty()
}

/// Removes one indent level from every line the selection touches, or from the
/// caret's line. Returns whether the text changed.
pub fn outdent(buffer: &mut Buffer, view: &mut View, options: &Options) -> bool {
    let (first, last) = selected_lines(buffer, view);
    let width = options.tab_width.max(1);
    let mut removals: Vec<(usize, usize)> = Vec::new();
    for line in first..=last {
        let start = buffer.line_start(line);
        let text = buffer.line_string(line);
        let mut count = 0;
        for character in text.chars().take(width) {
            if character == ' ' {
                count += 1;
            } else if character == '\t' {
                count += 1;
                break;
            } else {
                break;
            }
        }
        if count > 0 {
            removals.push((start, count));
        }
    }
    if removals.is_empty() {
        return false;
    }
    buffer.begin_edit();
    for (start, count) in removals.into_iter().rev() {
        buffer.remove(start..start + count, false);
        unshift(buffer, view, start, count);
    }
    buffer.end_edit();
    view.goal_col = None;
    true
}

/// Moves `view`'s caret and anchor by `delta` when they sit after `at`.
fn shift(buffer: &Buffer, view: &mut View, at: usize, delta: i64) {
    let len = buffer.len_chars() as i64;
    let apply = |position: usize| -> usize {
        let position = position as i64;
        if position > at as i64 {
            (position + delta).clamp(0, len) as usize
        } else {
            position as usize
        }
    };
    view.caret = apply(view.caret);
    view.anchor = apply(view.anchor);
}

/// Adjusts `view`'s caret and anchor after `count` chars were removed at `at`:
/// a position after the removed span moves back, one inside it snaps to `at`.
fn unshift(buffer: &Buffer, view: &mut View, at: usize, count: usize) {
    let len = buffer.len_chars();
    let end = at + count;
    let apply = |position: usize| -> usize {
        if position <= at {
            position
        } else if position >= end {
            position - count
        } else {
            at
        }
        .min(len)
    };
    view.caret = apply(view.caret);
    view.anchor = apply(view.anchor);
}

/// Copies the selection, or the caret's whole line when nothing is selected.
/// Returns whether anything was copied.
pub fn copy(buffer: &Buffer, view: &View, clipboard: &dyn Clipboard) -> bool {
    let text = match view.selection() {
        Some((start, end)) => buffer.slice(start..end),
        None => {
            let line = buffer.line_of_char(view.caret);
            let (_, end) = line_with_terminator(buffer, line);
            buffer.slice(buffer.line_start(line)..end)
        }
    };
    if text.is_empty() {
        return false;
    }
    clipboard.set_text(&text);
    true
}

/// Cuts the selection, or the caret's whole line, to the clipboard. Returns
/// whether the text changed.
pub fn cut(buffer: &mut Buffer, view: &mut View, clipboard: &dyn Clipboard) -> bool {
    match view.selection() {
        Some((start, end)) => {
            let text = buffer.slice(start..end);
            clipboard.set_text(&text);
            buffer.replace(start..end, "", false);
            view.caret = start;
        }
        None => {
            let line = buffer.line_of_char(view.caret);
            let start = buffer.line_start(line);
            let (_, end) = line_with_terminator(buffer, line);
            if start == end {
                return false;
            }
            let text = buffer.slice(start..end);
            clipboard.set_text(&text);
            buffer.remove(start..end, false);
            view.caret = start.min(buffer.len_chars());
        }
    }
    view.anchor = view.caret;
    view.goal_col = None;
    true
}

/// Pastes the clipboard at the caret, replacing the selection. Returns whether
/// the text changed.
pub fn paste(buffer: &mut Buffer, view: &mut View, clipboard: &dyn Clipboard) -> bool {
    let Some(text) = clipboard.text() else {
        return false;
    };
    if text.is_empty() {
        return false;
    }
    splice(buffer, view, &text, false);
    true
}

/// Undoes the last action. Returns whether the text changed.
pub fn undo(buffer: &mut Buffer, view: &mut View) -> bool {
    let Some(caret) = buffer.undo() else {
        return false;
    };
    view.caret = caret.min(buffer.len_chars());
    view.anchor = view.caret;
    view.goal_col = None;
    true
}

/// Redoes the last undone action. Returns whether the text changed.
pub fn redo(buffer: &mut Buffer, view: &mut View) -> bool {
    let Some(caret) = buffer.redo() else {
        return false;
    };
    view.caret = caret.min(buffer.len_chars());
    view.anchor = view.caret;
    view.goal_col = None;
    true
}

/// The end of `line` including its terminator, when it has one.
fn line_with_terminator(buffer: &Buffer, line: usize) -> (usize, usize) {
    let start = buffer.line_start(line);
    let count = buffer.line_count();
    let end = if line + 1 < count {
        buffer.line_start(line + 1)
    } else {
        buffer.len_chars()
    };
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::InProcessClipboard;

    fn editor(text: &str) -> (Buffer, View, Options) {
        let buffer = Buffer::new(text);
        let mut view = View::new();
        view.caret = buffer.len_chars();
        view.anchor = view.caret;
        (buffer, view, Options::default())
    }

    fn place(view: &mut View, caret: usize) {
        view.caret = caret;
        view.anchor = caret;
    }

    #[test]
    fn typing_replaces_the_selection() {
        let (mut buffer, mut view, _) = editor("hello world");
        view.anchor = 0;
        view.caret = 5;
        type_text(&mut buffer, &mut view, "bye");
        assert_eq!(buffer.text(), "bye world");
        assert_eq!(view.caret, 3);
        assert_eq!(view.selection(), None);
    }

    #[test]
    fn enter_copies_the_current_indent() {
        let (mut buffer, mut view, _) = editor("    let x = 1;");
        place(&mut view, 4);
        enter(&mut buffer, &mut view);
        assert_eq!(buffer.text(), "    \n    let x = 1;");
        assert_eq!(view.caret, 9);
    }

    #[test]
    fn enter_replaces_the_selection() {
        let (mut buffer, mut view, _) = editor("xabcx");
        view.anchor = 1;
        view.caret = 4;
        enter(&mut buffer, &mut view);
        assert_eq!(
            buffer.text(),
            "x
x"
        );
        assert_eq!(view.caret, 2);
        assert_eq!(view.selection(), None);
        undo(&mut buffer, &mut view);
        assert_eq!(buffer.text(), "xabcx", "one undo step");
    }

    #[test]
    fn indent_and_outdent_report_whether_the_text_changed() {
        let (mut buffer, mut view, options) = editor("a");
        place(&mut view, 0);
        assert!(indent(&mut buffer, &mut view, &options));
        assert!(outdent(&mut buffer, &mut view, &options));
        assert!(
            !outdent(&mut buffer, &mut view, &options),
            "nothing to remove"
        );
    }

    #[test]
    fn backspace_deletes_the_selection_or_one_char() {
        let (mut buffer, mut view, _) = editor("abc");
        view.anchor = 0;
        view.caret = 2;
        backspace(&mut buffer, &mut view);
        assert_eq!(buffer.text(), "c");
        assert_eq!(view.caret, 0);

        let (mut buffer, mut view, _) = editor("abc");
        place(&mut view, 3);
        backspace(&mut buffer, &mut view);
        assert_eq!(buffer.text(), "ab");
        assert_eq!(view.caret, 2);
    }

    #[test]
    fn delete_forward_removes_the_char_after_the_caret() {
        let (mut buffer, mut view, _) = editor("abc");
        place(&mut view, 1);
        delete_forward(&mut buffer, &mut view);
        assert_eq!(buffer.text(), "ac");
        assert_eq!(view.caret, 1);
    }

    #[test]
    fn indent_moves_every_selected_line_and_undoes_once() {
        let (mut buffer, mut view, options) = editor("a\nb\nc");
        view.anchor = 0;
        view.caret = buffer.len_chars();
        indent(&mut buffer, &mut view, &options);
        assert_eq!(buffer.text(), "    a\n    b\n    c");
        assert_eq!(view.anchor, 0);
        assert_eq!(view.caret, buffer.len_chars());
        undo(&mut buffer, &mut view);
        assert_eq!(buffer.text(), "a\nb\nc");
    }

    #[test]
    fn indent_without_a_selection_inserts_at_the_caret() {
        let (mut buffer, mut view, options) = editor("ab");
        place(&mut view, 1);
        indent(&mut buffer, &mut view, &options);
        assert_eq!(buffer.text(), "a    b");
        assert_eq!(view.caret, 5);
    }

    #[test]
    fn outdent_removes_a_level_from_each_selected_line() {
        let (mut buffer, mut view, options) = editor("    a\n\tb\nc");
        view.anchor = 0;
        view.caret = buffer.len_chars();
        outdent(&mut buffer, &mut view, &options);
        assert_eq!(buffer.text(), "a\nb\nc");
        undo(&mut buffer, &mut view);
        assert_eq!(buffer.text(), "    a\n\tb\nc");
    }

    #[test]
    fn outdent_without_a_selection_uses_the_caret_line() {
        let (mut buffer, mut view, options) = editor("one\n    two");
        place(&mut view, buffer.len_chars());
        outdent(&mut buffer, &mut view, &options);
        assert_eq!(buffer.text(), "one\ntwo");
    }

    #[test]
    fn copy_takes_the_selection_or_the_whole_line() {
        let clipboard = InProcessClipboard;
        let (buffer, mut view, _) = editor("one\ntwo");
        view.anchor = 0;
        view.caret = 3;
        assert!(copy(&buffer, &view, &clipboard));
        assert_eq!(clipboard.text().as_deref(), Some("one"));

        view.anchor = 4;
        view.caret = 4;
        assert!(copy(&buffer, &view, &clipboard));
        assert_eq!(clipboard.text().as_deref(), Some("two"));
    }

    #[test]
    fn cut_removes_the_selection_or_the_whole_line() {
        let clipboard = InProcessClipboard;
        let (mut buffer, mut view, _) = editor("one\ntwo");
        place(&mut view, 4);
        assert!(cut(&mut buffer, &mut view, &clipboard));
        assert_eq!(buffer.text(), "one\n");
        assert_eq!(clipboard.text().as_deref(), Some("two"));
        undo(&mut buffer, &mut view);
        assert_eq!(buffer.text(), "one\ntwo");
    }

    #[test]
    fn paste_replaces_the_selection() {
        let clipboard = InProcessClipboard;
        clipboard.set_text("X");
        let (mut buffer, mut view, _) = editor("one");
        place(&mut view, 1);
        assert!(paste(&mut buffer, &mut view, &clipboard));
        assert_eq!(buffer.text(), "oXne");
        assert_eq!(view.caret, 2);
    }

    #[test]
    fn undo_and_redo_drive_the_view_caret() {
        let (mut buffer, mut view, _) = editor("");
        type_text(&mut buffer, &mut view, "ab");
        undo(&mut buffer, &mut view);
        assert_eq!(buffer.text(), "");
        assert_eq!(view.caret, 0);
        redo(&mut buffer, &mut view);
        assert_eq!(buffer.text(), "ab");
        assert_eq!(view.caret, 2);
    }
}
