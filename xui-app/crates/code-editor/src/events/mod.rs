#![forbid(unsafe_code)]

//! The editor's event mapper: pointer and keyboard input to caret, selection,
//! scrolling and edits.
//!
//! The mapper is generic over the app's message type for the [`Ui`] it needs to
//! measure, focus and repaint; the widget wraps it and raises `on_change`.

mod scroll;

#[cfg(test)]
mod tests;

use xui_core::app::Ui;
use xui_core::backend::{Event, WidgetId};
use xui_core::geometry::Rect;
use xui_core::message::{Key, MouseButton};

use crate::edit;
use crate::metrics::{CELL_PROBE, Metrics, Viewport};
use crate::state::{EditorState, Effect};
use crate::text::char_col_for_display;
use crate::view::word_range_at;

use scroll::{scrollbar_down, scrollbar_move, wheel};

/// What an event did, so the widget knows whether to raise `on_change`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// Whether the text changed and `on_change` should fire.
    pub changed: bool,
}

/// The metrics and viewport for the current bounds and buffer.
fn viewport<M: 'static>(ui: &Ui<M>, id: WidgetId, state: &EditorState) -> Viewport {
    let dpi = ui.dpi();
    let style = state.options.font.style(xui_core::Color::rgb(0, 0, 0));
    let measured = ui.measure_text(CELL_PROBE, &style, dpi);
    let line_count = state.buffer.line_count();
    let metrics = Metrics::new(measured, line_count, state.options.show_gutter, dpi);
    Viewport::split(
        // Events are node-local, so the viewport sits at the node's origin.
        Rect::from_size(ui.bounds(id).size()),
        metrics,
        line_count,
        state.buffer.max_line_cols(state.options.tab_width),
        dpi,
    )
}

/// Handles one event, returning `None` when it is not the editor's.
///
/// After an event that changed the text, the highlight cache is brought up to
/// date from the earliest line the edit touched.
pub(crate) fn handle<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    id: WidgetId,
    event: &Event,
) -> Option<Outcome> {
    let outcome = dispatch(state, ui, id, event)?;
    if outcome.changed {
        state.sync_highlight();
    }
    Some(outcome)
}

/// Dispatches one event to the editor's input rules.
fn dispatch<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    id: WidgetId,
    event: &Event,
) -> Option<Outcome> {
    match event {
        Event::SetFocus => {
            state.focused = true;
            state.reset_blink();
            Some(Outcome::default())
        }
        Event::KillFocus => {
            state.focused = false;
            state.view.dragging = false;
            Some(Outcome::default())
        }
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            modifiers,
        } => Some(mouse_press(state, ui, id, *x, *y, modifiers.shift, None)),
        Event::MouseDoubleClick {
            x,
            y,
            button: MouseButton::Left,
            ..
        } => Some(mouse_press(state, ui, id, *x, *y, false, Some(2))),
        Event::MouseMove { x, y, .. } => Some(mouse_move(state, ui, id, *x, *y)),
        Event::MouseUp {
            button: MouseButton::Left,
            ..
        } => {
            state.view.dragging = false;
            state.v_drag = None;
            state.h_drag = None;
            if state.captured {
                state.captured = false;
                state.effects.push(Effect::ReleaseCapture);
            }
            Some(Outcome::default())
        }
        Event::CaptureChanged => {
            state.view.dragging = false;
            state.v_drag = None;
            state.h_drag = None;
            state.captured = false;
            Some(Outcome::default())
        }
        Event::MouseWheel {
            delta,
            horizontal,
            modifiers,
            ..
        } => Some(wheel(state, ui, id, *delta, *horizontal, modifiers.shift)),
        Event::KeyDown {
            key,
            modifiers,
            system,
            ..
        } if !*system => {
            if !state.focused {
                return None;
            }
            key_down(state, ui, id, *key, modifiers.ctrl, modifiers.shift)
        }
        Event::Char(character) if state.focused => {
            if character.is_control() {
                return None;
            }
            edit::type_char(&mut state.buffer, &mut state.view, *character);
            finish_edit(state, ui, id);
            Some(Outcome { changed: true })
        }
        Event::Timer { .. } => {
            if state.focused {
                state.toggle_blink();
            }
            Some(Outcome::default())
        }
        Event::Resize { .. } => {
            ensure_visible(state, ui, id);
            Some(Outcome::default())
        }
        _ => None,
    }
}

/// Handles a left-button press: caret placement, word/line selection or a
/// scrollbar drag. `forced_count` overrides the tracked click run when a
/// backend reports a double click as its own event.
fn mouse_press<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    id: WidgetId,
    x: i32,
    y: i32,
    shift: bool,
    forced_count: Option<u8>,
) -> Outcome {
    state.effects.push(Effect::Focus);
    state.focused = true;
    let layout = viewport(ui, id, state);
    let first_line = clamped_first_line(state, &layout);
    if scrollbar_down(state, ui, &layout, first_line, x, y) {
        return Outcome::default();
    }

    let count = match forced_count {
        Some(count) => {
            state.click.set_count(count, x, y);
            count
        }
        None => state.click.register(x, y),
    };
    state.buffer.break_coalescing();
    let position = position_at(state, &layout, first_line, x, y);
    match count {
        2 => {
            let line = state.buffer.line_of_char(position);
            let column = layout.metrics.col_at(layout.text, x, state.view.first_col);
            let (start, end) = word_range_at(&state.buffer, line, column, state.options.tab_width);
            state.view.anchor = start;
            state.view.caret = end;
            state.view.goal_col = None;
            state.view.dragging = true;
        }
        3 => {
            let line = state.buffer.line_of_char(position);
            let start = state.buffer.line_start(line);
            let end = if line + 1 < state.buffer.line_count() {
                state.buffer.line_start(line + 1)
            } else {
                state.buffer.len_chars()
            };
            state.view.anchor = start;
            state.view.caret = end;
            state.view.goal_col = None;
            state.view.dragging = false;
        }
        _ => {
            state.view.caret = position;
            if !shift {
                state.view.anchor = position;
            }
            state.view.goal_col = None;
            state.view.dragging = true;
        }
    }
    state.captured = true;
    state.effects.push(Effect::Capture);
    state.reset_blink();
    ensure_visible(state, ui, id);
    Outcome::default()
}

/// Handles a mouse move during a text or scrollbar drag.
fn mouse_move<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    id: WidgetId,
    x: i32,
    y: i32,
) -> Outcome {
    let layout = viewport(ui, id, state);
    let first_line = clamped_first_line(state, &layout);
    if scrollbar_move(state, ui, id, &layout, first_line, x, y) {
        return Outcome::default();
    }
    if state.view.dragging {
        state.buffer.break_coalescing();
        let position = position_at(state, &layout, first_line, x, y);
        state.view.caret = position;
        state.view.goal_col = None;
        state.reset_blink();
        ensure_visible(state, ui, id);
    }
    Outcome::default()
}
/// Handles a navigation or editing key.
fn key_down<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    id: WidgetId,
    key: Key,
    ctrl: bool,
    shift: bool,
) -> Option<Outcome> {
    let tab = state.options.tab_width;
    let layout = viewport(ui, id, state);
    let page = layout.visible_lines as i64;
    // Whether the text changed is read off the buffer afterwards, not assumed
    // per key: Backspace at the top or Delete at the end changes nothing, and
    // Tab indents without being an "edit key".
    let revision = state.buffer.revision();
    if key != Key::BACK && key != Key::DELETE {
        state.buffer.break_coalescing();
    }
    match key {
        Key::LEFT if ctrl => state.view.word_left(&state.buffer, shift),
        Key::LEFT => state.view.left(shift),
        Key::RIGHT if ctrl => state.view.word_right(&state.buffer, shift),
        Key::RIGHT => state.view.right(&state.buffer, shift),
        Key::UP => state.view.up(&state.buffer, tab, shift),
        Key::DOWN => state.view.down(&state.buffer, tab, shift),
        Key::HOME if ctrl => state.view.document_home(shift),
        Key::HOME => state.view.home(&state.buffer, shift),
        Key::END if ctrl => state.view.document_end(&state.buffer, shift),
        Key::END => state.view.end(&state.buffer, shift),
        Key::PAGE_UP => state.view.page(&state.buffer, tab, -page, shift),
        Key::PAGE_DOWN => state.view.page(&state.buffer, tab, page, shift),
        Key::BACK => {
            edit::backspace(&mut state.buffer, &mut state.view);
        }
        Key::DELETE => {
            edit::delete_forward(&mut state.buffer, &mut state.view);
        }
        Key::RETURN => {
            edit::enter(&mut state.buffer, &mut state.view);
        }
        Key::TAB if shift => {
            edit::outdent(&mut state.buffer, &mut state.view, &state.options);
        }
        Key::TAB => {
            edit::indent(&mut state.buffer, &mut state.view, &state.options);
        }
        Key::A if ctrl => state.view.select_all(&state.buffer),
        Key::C if ctrl => {
            edit::copy(&state.buffer, &state.view, state.clipboard.as_ref());
        }
        Key::X if ctrl => {
            edit::cut(&mut state.buffer, &mut state.view, state.clipboard.as_ref());
        }
        Key::V if ctrl => {
            edit::paste(&mut state.buffer, &mut state.view, state.clipboard.as_ref());
        }
        Key::Z if ctrl && shift => {
            edit::redo(&mut state.buffer, &mut state.view);
        }
        Key::Z if ctrl => {
            edit::undo(&mut state.buffer, &mut state.view);
        }
        Key::Y if ctrl => {
            edit::redo(&mut state.buffer, &mut state.view);
        }
        Key::ESCAPE => {
            state.view.collapse();
            state.view.dragging = false;
        }
        _ => return None,
    }
    let changed = state.buffer.revision() != revision;
    finish_edit(state, ui, id);
    Some(Outcome { changed })
}

/// Resets the blink and scrolls the caret into view after an edit or move.
fn finish_edit<M: 'static>(state: &mut EditorState, ui: &Ui<M>, id: WidgetId) {
    state.reset_blink();
    ensure_visible(state, ui, id);
}

/// Scrolls so the caret is visible, then clamps both axes to their content.
pub(crate) fn ensure_visible<M: 'static>(state: &mut EditorState, ui: &Ui<M>, id: WidgetId) {
    let layout = viewport(ui, id, state);
    let tab = state.options.tab_width;
    state.view.ensure_caret_visible(
        &state.buffer,
        tab,
        layout.visible_lines,
        layout.visible_cols,
    );
    let max_line = state
        .buffer
        .line_count()
        .saturating_sub(layout.visible_lines);
    state.view.first_line = state.view.first_line.min(max_line);
    let max_col = state
        .buffer
        .max_line_cols(state.options.tab_width)
        .saturating_sub(layout.visible_cols);
    state.view.first_col = state.view.first_col.min(max_col);
}

/// The buffer position at a point.
fn position_at(state: &EditorState, layout: &Viewport, first_line: usize, x: i32, y: i32) -> usize {
    let line_count = state.buffer.line_count();
    let line = layout
        .metrics
        .line_at(layout.text, y, first_line, line_count);
    let column = layout.metrics.col_at(layout.text, x, state.view.first_col);
    let text = state.buffer.line_string(line);
    let char_col = char_col_for_display(&text, column, state.options.tab_width);
    state.buffer.line_start(line) + char_col
}

/// The first visible line, clamped to the buffer.
fn clamped_first_line(state: &EditorState, layout: &Viewport) -> usize {
    state.view.first_line.min(
        state
            .buffer
            .line_count()
            .saturating_sub(layout.visible_lines),
    )
}
