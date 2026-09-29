#![forbid(unsafe_code)]

//! The editor's event mapper: pointer and keyboard input to caret, selection,
//! scrolling and edits.
//!
//! The mapper is generic over the app's message type for the [`Ui`] it needs to
//! measure, focus and repaint; the widget wraps it and raises `on_change`.

use xui_core::app::Ui;
use xui_core::backend::{Event, WidgetId};
use xui_core::geometry::Rect;
use xui_core::message::{Key, MouseButton};

use crate::edit;
use crate::metrics::{CELL_PROBE, Metrics, Viewport};
use crate::state::{Drag, EditorState, Effect};
use crate::text::char_col_for_display;
use crate::view::word_range_at;
use xui_core::widget::scrollbar::{self, Orientation, Scroll};

/// Rows a wheel notch scrolls.
const WHEEL_ROWS: i32 = 3;
/// Columns a horizontal wheel notch scrolls.
const WHEEL_COLS: i32 = 3;
/// The wheel delta of one notch. Both backends report `WHEEL_DELTA` units:
/// Win32 passes them through and the canvas backend scales winit's line
/// deltas by it (and passes a touchpad's pixel deltas as-is).
const WHEEL_NOTCH: i32 = 120;

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

/// Handles a wheel event, vertically or horizontally.
fn wheel<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    id: WidgetId,
    delta: i16,
    horizontal: bool,
    shift: bool,
) -> Outcome {
    let layout = viewport(ui, id, state);
    if horizontal || shift {
        let max = max_first_col(state, &layout).max(0) as usize;
        state.view.first_col = wheel_scroll(
            state.view.first_col,
            &mut state.wheel_cols_rest,
            delta,
            WHEEL_COLS,
            max,
        );
    } else {
        let max = state
            .buffer
            .line_count()
            .saturating_sub(layout.visible_lines);
        state.view.first_line = wheel_scroll(
            state.view.first_line,
            &mut state.wheel_rows_rest,
            delta,
            WHEEL_ROWS,
            max,
        );
        state.view.goal_col = None;
    }
    Outcome::default()
}

/// The first visible line (or column) after a wheel `delta`: `per_notch`
/// steps per [`WHEEL_NOTCH`], proportionally for partial (touchpad) deltas,
/// clamped to `0..=max`.
///
/// Travel short of a whole step is kept in `rest` and added to the next
/// event, so a run of small deltas scrolls instead of rounding to nothing. A
/// positive delta is "away from the user", which shows earlier lines, so it
/// scrolls up.
fn wheel_scroll(first: usize, rest: &mut i32, delta: i16, per_notch: i32, max: usize) -> usize {
    let units = *rest + i32::from(delta) * per_notch;
    *rest = units % WHEEL_NOTCH;
    let target = first as i64 - i64::from(units / WHEEL_NOTCH);
    let clamped = target.clamp(0, max as i64);
    // At an end, leftover travel pointing past it is dropped, so reversing
    // direction scrolls at once instead of first unwinding that travel.
    let outward = (clamped == 0 && *rest > 0) || (clamped == max as i64 && *rest < 0);
    if clamped != target || outward {
        *rest = 0;
    }
    clamped as usize
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

/// Starts a scrollbar drag or a page jump when `(x, y)` is on a bar, returning
/// whether it was handled.
fn scrollbar_down<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    layout: &Viewport,
    first_line: usize,
    x: i32,
    y: i32,
) -> bool {
    let dpi = ui.dpi();
    if let Some(track) = layout.vbar
        && track.contains(xui_core::geometry::Point::new(x, y))
    {
        let scroll = vertical_scroll(state, layout, first_line);
        match scrollbar::thumb(track, scroll, Orientation::Vertical, dpi) {
            Some(thumb) if y >= thumb.top && y < thumb.bottom => {
                state.v_drag = Some(Drag {
                    start_offset: first_line as i32 * layout.metrics.line_height,
                    start_pointer: y,
                });
            }
            thumb => {
                let page = layout.visible_lines as i32;
                let above = thumb.is_some_and(|thumb| y < thumb.top);
                let target = if above {
                    first_line as i32 - page
                } else {
                    first_line as i32 + page
                };
                let max = state
                    .buffer
                    .line_count()
                    .saturating_sub(layout.visible_lines);
                state.view.first_line = target.clamp(0, max as i32) as usize;
            }
        }
        state.captured = true;
        state.effects.push(Effect::Capture);
        return true;
    }
    if let Some(track) = layout.hbar
        && track.contains(xui_core::geometry::Point::new(x, y))
    {
        let scroll = horizontal_scroll(state, layout);
        match scrollbar::thumb(track, scroll, Orientation::Horizontal, dpi) {
            Some(thumb) if x >= thumb.left && x < thumb.right => {
                state.h_drag = Some(Drag {
                    start_offset: state.view.first_col as i32 * layout.metrics.advance,
                    start_pointer: x,
                });
            }
            thumb => {
                let page = layout.visible_cols as i32;
                let before = thumb.is_some_and(|thumb| x < thumb.left);
                let target = if before {
                    state.view.first_col as i32 - page
                } else {
                    state.view.first_col as i32 + page
                };
                state.view.first_col = target.clamp(0, max_first_col(state, layout)) as usize;
            }
        }
        state.captured = true;
        state.effects.push(Effect::Capture);
        return true;
    }
    false
}

/// Applies an in-progress scrollbar drag, returning whether it is dragging.
fn scrollbar_move<M: 'static>(
    state: &mut EditorState,
    ui: &Ui<M>,
    _id: WidgetId,
    layout: &Viewport,
    first_line: usize,
    x: i32,
    y: i32,
) -> bool {
    let dpi = ui.dpi();
    let metrics = layout.metrics;
    if let (Some(drag), Some(track)) = (state.v_drag, layout.vbar) {
        let scroll = vertical_scroll(state, layout, first_line);
        let pixels = scrollbar::offset_from_drag(
            track,
            scroll,
            Orientation::Vertical,
            drag.start_offset,
            drag.start_pointer,
            y,
            dpi,
        );
        let max = state
            .buffer
            .line_count()
            .saturating_sub(layout.visible_lines);
        state.view.first_line = ((pixels / metrics.line_height).max(0) as usize).min(max);
        return true;
    }
    if let (Some(drag), Some(track)) = (state.h_drag, layout.hbar) {
        let scroll = horizontal_scroll(state, layout);
        let pixels = scrollbar::offset_from_drag(
            track,
            scroll,
            Orientation::Horizontal,
            drag.start_offset,
            drag.start_pointer,
            x,
            dpi,
        );
        state.view.first_col = (pixels / metrics.advance).max(0) as usize;
        return true;
    }
    false
}

/// The vertical scroll state for `first_line`.
fn vertical_scroll(state: &EditorState, layout: &Viewport, first_line: usize) -> Scroll {
    Scroll {
        viewport: layout.text.height(),
        content: state.buffer.line_count() as i32 * layout.metrics.line_height,
        offset: first_line as i32 * layout.metrics.line_height,
    }
}

/// The horizontal scroll state.
/// The largest `first_col` that still shows text: the longest line's length
/// minus the visible columns.
fn max_first_col(state: &EditorState, layout: &Viewport) -> i32 {
    state
        .buffer
        .max_line_cols(state.options.tab_width)
        .saturating_sub(layout.visible_cols) as i32
}

fn horizontal_scroll(state: &EditorState, layout: &Viewport) -> Scroll {
    Scroll {
        viewport: layout.text.width(),
        content: state.buffer.max_line_cols(state.options.tab_width) as i32
            * layout.metrics.advance,
        offset: state.view.first_col as i32 * layout.metrics.advance,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use xui_canvas::OffscreenBackend;
    use xui_core::backend::{Event, NodeKind, NodeSpec, PlatformSpec};
    use xui_core::geometry::Rect;
    use xui_core::message::{Modifiers, MouseButton};
    use xui_core::units::Dip;
    use xui_core::{App, Ui, run_app};

    use super::{WHEEL_COLS, WHEEL_ROWS, handle, max_first_col, viewport, wheel_scroll};
    use crate::options::Options;
    use crate::platform::InProcessClipboard;
    use crate::state::{EditorState, Effect};

    struct Empty;

    impl App for Empty {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
    }

    /// Runs `check` with a live offscreen `Ui` and an editor-sized node.
    fn with_ui(check: impl FnOnce(&Ui<()>, xui_core::backend::WidgetId) + 'static) {
        let check = Rc::new(RefCell::new(Some(check)));
        run_app(
            Rc::new(OffscreenBackend::new()),
            PlatformSpec::new("events").size(Dip(300.0), Dip(200.0)),
            move |ui| {
                let id = ui
                    .create_node(&NodeSpec::new(NodeKind::Custom, Rect::new(0, 0, 300, 200)))
                    .expect("node");
                if let Some(check) = check.borrow_mut().take() {
                    check(ui, id);
                }
                Empty
            },
        )
        .expect("run_app");
    }

    fn state(text: &str) -> EditorState {
        EditorState::new(text, Options::default(), Box::new(InProcessClipboard))
    }

    /// A state that highlights Rhai, for the token-sync tests.
    #[cfg(feature = "rhai-syntax")]
    fn rhai_state(text: &str) -> EditorState {
        EditorState::with_highlighter(
            text,
            Options::default(),
            Box::new(InProcessClipboard),
            Box::new(crate::lexer::RhaiHighlighter),
        )
    }

    #[test]
    fn focus_and_capture_are_deferred_not_called_under_the_borrow() {
        // The canvas backend delivers SetFocus / CaptureChanged synchronously
        // back into the mapper, so the handler must only record them.
        with_ui(|ui, id| {
            let mut state = state("hello\nworld");
            let press = Event::MouseDown {
                x: 80,
                y: 5,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            };
            handle(&mut state, ui, id, &press).expect("press is handled");
            assert_eq!(state.effects, [Effect::Focus, Effect::Capture]);
            assert!(state.focused);

            state.effects.clear();
            let release = Event::MouseUp {
                x: 80,
                y: 5,
                button: MouseButton::Left,
                modifiers: Modifiers::default(),
            };
            handle(&mut state, ui, id, &release).expect("release is handled");
            assert_eq!(state.effects, [Effect::ReleaseCapture]);
        });
    }

    /// A clipboard shared with the test, so it can see what was copied and
    /// choose what is pasted.
    #[derive(Clone, Default)]
    struct Recording(Rc<RefCell<String>>);

    impl crate::platform::Clipboard for Recording {
        fn text(&self) -> Option<String> {
            let text = self.0.borrow();
            (!text.is_empty()).then(|| text.clone())
        }

        fn set_text(&self, text: &str) {
            *self.0.borrow_mut() = text.to_owned();
        }
    }

    fn ctrl(key: xui_core::message::Key) -> Event {
        Event::KeyDown {
            key,
            modifiers: xui_core::message::Modifiers {
                ctrl: true,
                ..xui_core::message::Modifiers::NONE
            },
            repeat: 1,
            system: false,
        }
    }

    fn key_down(key: xui_core::message::Key) -> Event {
        Event::KeyDown {
            key,
            modifiers: xui_core::message::Modifiers::NONE,
            repeat: 1,
            system: false,
        }
    }

    #[test]
    fn keys_that_change_nothing_do_not_report_a_change() {
        use xui_core::message::Key;

        with_ui(|ui, id| {
            let mut state = state("hello");
            handle(&mut state, ui, id, &Event::SetFocus);
            // Home, then Backspace at the top: the text is untouched.
            handle(&mut state, ui, id, &key_down(Key::HOME));
            let outcome = handle(&mut state, ui, id, &key_down(Key::BACK)).expect("handled");
            assert!(!outcome.changed, "Backspace at the start deletes nothing");
            handle(&mut state, ui, id, &key_down(Key::END));
            let outcome = handle(&mut state, ui, id, &key_down(Key::DELETE)).expect("handled");
            assert!(!outcome.changed, "Delete at the end deletes nothing");
            // Undo with no history is not a change either.
            let outcome = handle(&mut state, ui, id, &ctrl(Key::Z)).expect("handled");
            assert!(!outcome.changed);
            // A copy with the selection empty leaves the text alone.
            let outcome = handle(&mut state, ui, id, &ctrl(Key::C)).expect("handled");
            assert!(!outcome.changed);
        });
    }

    #[test]
    fn edits_undo_and_redo_report_a_change() {
        use xui_core::message::Key;

        with_ui(|ui, id| {
            let mut state = state("hello");
            handle(&mut state, ui, id, &Event::SetFocus);
            handle(&mut state, ui, id, &key_down(Key::END));
            let outcome = handle(&mut state, ui, id, &key_down(Key::BACK)).expect("handled");
            assert!(outcome.changed);
            assert_eq!(state.buffer.text(), "hell");
            let outcome = handle(&mut state, ui, id, &ctrl(Key::Z)).expect("handled");
            assert!(outcome.changed);
            assert_eq!(state.buffer.text(), "hello");
            let outcome = handle(&mut state, ui, id, &ctrl(Key::Y)).expect("handled");
            assert!(outcome.changed);
            assert_eq!(state.buffer.text(), "hell");
        });
    }

    #[test]
    fn copy_and_paste_go_through_an_injected_clipboard() {
        use xui_core::message::Key;

        with_ui(|ui, id| {
            let clipboard = Recording::default();
            let mut state =
                EditorState::new("hello", Options::default(), Box::new(clipboard.clone()));
            handle(&mut state, ui, id, &Event::SetFocus);
            handle(&mut state, ui, id, &ctrl(Key::A));
            handle(&mut state, ui, id, &ctrl(Key::C));
            assert_eq!(
                *clipboard.0.borrow(),
                "hello",
                "copy reached the injected clipboard"
            );

            *clipboard.0.borrow_mut() = "bye".to_owned();
            handle(&mut state, ui, id, &ctrl(Key::A));
            handle(&mut state, ui, id, &ctrl(Key::V));
            assert_eq!(
                state.buffer.text(),
                "bye",
                "paste read the injected clipboard"
            );
        });
    }

    #[test]
    fn tab_and_shift_tab_report_a_change() {
        use xui_core::message::{Key, Modifiers};

        with_ui(|ui, id| {
            let mut state = state("a");
            handle(&mut state, ui, id, &Event::SetFocus);
            let key = |shift| Event::KeyDown {
                key: Key::TAB,
                modifiers: Modifiers {
                    shift,
                    ..Modifiers::NONE
                },
                repeat: 1,
                system: false,
            };
            let indented = handle(&mut state, ui, id, &key(false)).expect("tab");
            assert!(indented.changed);
            let outdented = handle(&mut state, ui, id, &key(true)).expect("shift+tab");
            assert!(outdented.changed);
            let nothing = handle(&mut state, ui, id, &key(true)).expect("shift+tab");
            assert!(!nothing.changed, "no indent left to remove");
        });
    }

    #[test]
    fn typing_while_focused_changes_the_text() {
        with_ui(|ui, id| {
            let mut state = state("");
            handle(&mut state, ui, id, &Event::SetFocus);
            let outcome = handle(&mut state, ui, id, &Event::Char('x')).expect("char is handled");
            assert!(outcome.changed);
            assert_eq!(state.buffer.text(), "x");
        });
    }

    #[cfg(feature = "rhai-syntax")]
    #[test]
    fn typing_keeps_the_highlight_in_sync() {
        use crate::lexer::TokenClass;

        with_ui(|ui, id| {
            let mut state = rhai_state("let x = 1;");
            handle(&mut state, ui, id, &Event::SetFocus);
            handle(&mut state, ui, id, &Event::Char('/')).expect("first slash");
            handle(&mut state, ui, id, &Event::Char('/')).expect("second slash");
            assert_eq!(state.buffer.text(), "//let x = 1;");
            let classes: Vec<TokenClass> = state
                .highlight
                .tokens(0)
                .iter()
                .map(|token| token.class)
                .collect();
            assert_eq!(classes, [TokenClass::Comment]);
        });
    }

    /// A wheel event of `delta` units.
    fn wheel(delta: i16, horizontal: bool, shift: bool) -> Event {
        Event::MouseWheel {
            delta,
            horizontal,
            x: 80,
            y: 80,
            modifiers: Modifiers {
                shift,
                ..Modifiers::default()
            },
        }
    }

    /// A long, wide document that scrolls both ways.
    fn tall_state() -> EditorState {
        let line = "x".repeat(200);
        let text = vec![line.as_str(); 200].join("\n");
        state(&text)
    }

    #[test]
    fn one_notch_toward_the_user_scrolls_down_by_the_wheel_rows() {
        with_ui(|ui, id| {
            let mut state = tall_state();
            handle(&mut state, ui, id, &wheel(-120, false, false)).expect("wheel is handled");
            assert_eq!(state.view.first_line, WHEEL_ROWS as usize);
        });
    }

    #[test]
    fn one_notch_away_from_the_user_scrolls_up() {
        with_ui(|ui, id| {
            let mut state = tall_state();
            state.view.first_line = 10;
            handle(&mut state, ui, id, &wheel(120, false, false));
            assert_eq!(state.view.first_line, 10 - WHEEL_ROWS as usize);
        });
    }

    #[test]
    fn two_notches_scroll_twice_as_far() {
        with_ui(|ui, id| {
            let mut state = tall_state();
            handle(&mut state, ui, id, &wheel(-240, false, false));
            assert_eq!(state.view.first_line, 2 * WHEEL_ROWS as usize);
        });
    }

    #[test]
    fn partial_deltas_add_up_to_a_notch() {
        with_ui(|ui, id| {
            let mut state = tall_state();
            for _ in 0..3 {
                handle(&mut state, ui, id, &wheel(-40, false, false));
            }
            assert_eq!(state.view.first_line, WHEEL_ROWS as usize);
            assert_eq!(state.wheel_rows_rest, 0);
        });
    }

    #[test]
    fn tiny_touchpad_deltas_eventually_scroll() {
        with_ui(|ui, id| {
            let mut state = tall_state();
            handle(&mut state, ui, id, &wheel(-10, false, false));
            assert_eq!(state.view.first_line, 0, "a sliver of a line waits");
            for _ in 0..3 {
                handle(&mut state, ui, id, &wheel(-10, false, false));
            }
            assert_eq!(state.view.first_line, 1, "four slivers make a line");
        });
    }

    #[test]
    fn wheel_scrolling_clamps_at_the_top() {
        with_ui(|ui, id| {
            let mut state = tall_state();
            state.view.first_line = 1;
            handle(&mut state, ui, id, &wheel(120, false, false));
            assert_eq!(state.view.first_line, 0);
            assert_eq!(state.wheel_rows_rest, 0);
        });
    }

    #[test]
    fn wheel_scrolling_clamps_at_the_bottom() {
        with_ui(|ui, id| {
            let mut state = tall_state();
            let layout = viewport(ui, id, &state);
            let max = state.buffer.line_count() - layout.visible_lines;
            state.view.first_line = max - 1;
            handle(&mut state, ui, id, &wheel(-120, false, false));
            assert_eq!(state.view.first_line, max);
            handle(&mut state, ui, id, &wheel(-120 * 100, false, false));
            assert_eq!(state.view.first_line, max);
        });
    }

    #[test]
    fn shift_wheel_scrolls_horizontally() {
        with_ui(|ui, id| {
            let mut state = tall_state();
            handle(&mut state, ui, id, &wheel(-120, false, true));
            assert_eq!(state.view.first_col, WHEEL_COLS as usize);
            assert_eq!(state.view.first_line, 0);
            handle(&mut state, ui, id, &wheel(120, false, true));
            assert_eq!(state.view.first_col, 0);
        });
    }

    #[test]
    fn a_horizontal_wheel_scrolls_columns_and_clamps() {
        with_ui(|ui, id| {
            let mut state = tall_state();
            let max = max_first_col(&state, &viewport(ui, id, &state)) as usize;
            handle(&mut state, ui, id, &wheel(-120, true, false));
            assert_eq!(state.view.first_col, WHEEL_COLS as usize);
            handle(&mut state, ui, id, &wheel(-120 * 200, true, false));
            assert_eq!(state.view.first_col, max);
            handle(&mut state, ui, id, &wheel(120 * 200, true, false));
            assert_eq!(state.view.first_col, 0);
        });
    }

    #[test]
    fn wheel_scroll_carries_the_remainder_and_drops_it_at_an_end() {
        let mut rest = 0;
        assert_eq!(wheel_scroll(5, &mut rest, -20, 3, 50), 5);
        assert_eq!(rest, -60);
        assert_eq!(wheel_scroll(5, &mut rest, -20, 3, 50), 6);
        assert_eq!(rest, 0);
        assert_eq!(wheel_scroll(0, &mut rest, 20, 3, 50), 0);
        assert_eq!(rest, 0, "partial travel past the top is dropped");
        assert_eq!(
            wheel_scroll(0, &mut rest, -40, 3, 50),
            1,
            "reversing at the top scrolls at once"
        );
        assert_eq!(wheel_scroll(50, &mut rest, -20, 3, 50), 50);
        assert_eq!(rest, 0, "partial travel past the bottom is dropped");
        assert_eq!(
            wheel_scroll(50, &mut rest, 40, 3, 50),
            49,
            "reversing at the bottom scrolls at once"
        );
        assert_eq!(wheel_scroll(49, &mut rest, -140, 3, 50), 50);
        assert_eq!(rest, 0, "landing exactly on the bottom drops the rest");
    }
}
