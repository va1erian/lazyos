#![forbid(unsafe_code)]

//! The [`Editor`] widget: a single `NodeKind::Custom` xui node with a painter
//! and an event mapper.
//!
//! The widget owns a [`Control`], the shared [`EditorState`] and the app's
//! `on_change` mapper. Everything the app configures goes through this type;
//! the pure buffer, view and edit rules live in their own modules.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::{Cursor, NodeKind, NodeSpec, Result, WidgetId};
use xui_core::geometry::Rect;

use crate::buffer::Buffer;
use crate::edit;
use crate::events;
use crate::find;
use crate::lexer::Highlighter;
use crate::markers::Marker;
use crate::options::Options;
use crate::paint;
use crate::platform;
use crate::platform::Clipboard;
use crate::state::{EditorState, Effect};
use crate::theme::EditorTheme;
use crate::view::View;

/// How often the caret blinks, in milliseconds.
const BLINK_MS: u32 = 500;

/// Maps the new text to an optional app message.
type ChangeMapper<M> = Box<dyn Fn(&str) -> Option<M>>;

/// A code editor on a custom xui node.
pub struct Editor<M: 'static> {
    control: xui_core::widget::Control<M>,
    state: Rc<RefCell<EditorState>>,
    on_change: Rc<RefCell<Option<ChangeMapper<M>>>>,
}

impl<M: 'static> Editor<M> {
    /// Creates an editor at `bounds` with the default options.
    pub fn new(ui: &Ui<M>, bounds: Rect) -> Result<Editor<M>> {
        Editor::with_options(ui, bounds, Options::default())
    }

    /// Creates an editor at `bounds` with `options`.
    pub fn with_options(ui: &Ui<M>, bounds: Rect, options: Options) -> Result<Editor<M>> {
        let control = xui_core::widget::Control::new(
            ui,
            &NodeSpec::new(NodeKind::Custom, bounds).tab_stop(),
        )?;
        ui.set_cursor(control.id(), Cursor::Text);

        let state = Rc::new(RefCell::new(EditorState::new(
            "",
            options,
            platform::clipboard(ui),
        )));
        let on_change: Rc<RefCell<Option<ChangeMapper<M>>>> = Rc::new(RefCell::new(None));

        {
            let state = Rc::clone(&state);
            let theme = ui.theme_handle();
            control.set_painter(Rc::new(move |canvas| {
                let xui_theme = theme.get();
                let editor_theme = EditorTheme::from_theme(xui_theme);
                let state = state.borrow();
                paint::paint(canvas, &state, &editor_theme, &xui_theme);
            }));
        }
        {
            let state = Rc::clone(&state);
            let on_change = Rc::clone(&on_change);
            let ui = ui.clone();
            let id = control.id();
            control.on_events(move |event| {
                // In design mode the form editor handles input, not the widget.
                if ui.is_design_mode() && event.is_input() {
                    return None;
                }
                let (outcome, effects) = {
                    let mut state = state.borrow_mut();
                    let outcome = events::handle(&mut state, &ui, id, event);
                    (outcome, std::mem::take(&mut state.effects))
                };
                // Outside the borrow: these calls deliver events straight back
                // into this mapper (see `Effect`).
                for effect in effects {
                    match effect {
                        Effect::Focus => ui.focus(id),
                        Effect::Capture => ui.set_capture(id),
                        Effect::ReleaseCapture => ui.release_capture(),
                    }
                }
                let outcome = outcome?;
                ui.invalidate(id);
                if outcome.changed {
                    let text = state.borrow().buffer.text();
                    let mapper = on_change.borrow();
                    if let Some(mapper) = mapper.as_ref() {
                        return mapper(&text);
                    }
                }
                None
            });
        }

        // The caret blinks on the control's own timer (xui's per-widget timers),
        // so the host has nothing to forward; the control stops it on drop.
        {
            let state = Rc::clone(&state);
            let ui = ui.clone();
            let id = control.id();
            let _ = control.set_timer(BLINK_MS, move || {
                let mut state = state.borrow_mut();
                if state.focused {
                    state.toggle_blink();
                    drop(state);
                    ui.invalidate(id);
                }
                None
            });
        }

        Ok(Editor {
            control,
            state,
            on_change,
        })
    }

    /// Replaces the highlighter, re-lexing the whole buffer with it.
    ///
    /// [`Editor::new`] starts with [`PlainText`](crate::PlainText); pass a
    /// language highlighter here to colour the text.
    pub fn with_highlighter(self, highlighter: impl Highlighter + 'static) -> Editor<M> {
        self.set_highlighter(highlighter);
        self
    }

    /// Replaces the clipboard the editor copies, cuts and pastes through.
    ///
    /// [`Editor::new`] uses [`platform::clipboard`](crate::platform::clipboard):
    /// the window's portable clipboard (`Ui::clipboard_text`). An app that
    /// wants another store implements [`Clipboard`](crate::Clipboard) and
    /// passes it here.
    pub fn with_clipboard(self, clipboard: impl Clipboard + 'static) -> Editor<M> {
        self.set_clipboard(clipboard);
        self
    }

    /// Replaces the clipboard on a live editor, like
    /// [`Editor::with_clipboard`] for an editor that is already built.
    pub fn set_clipboard(&self, clipboard: impl Clipboard + 'static) {
        self.state.borrow_mut().clipboard = Box::new(clipboard);
    }

    /// Replaces the highlighter on a live editor, re-lexing the whole buffer.
    pub fn set_highlighter(&self, highlighter: impl Highlighter + 'static) {
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            state.set_highlighter(Box::new(highlighter));
        }
        self.control.invalidate();
    }

    /// Maps a text change to the app's message. The closure returns `Some(msg)`
    /// to raise it, or `None` to ignore the change; it receives the new text.
    pub fn on_change(self, mapper: impl Fn(&str) -> Option<M> + 'static) -> Editor<M> {
        *self.on_change.borrow_mut() = Some(Box::new(mapper));
        self
    }

    /// The widget's node identity.
    pub fn id(&self) -> WidgetId {
        self.control.id()
    }

    /// Gives the editor the keyboard focus.
    pub fn focus(&self) {
        self.control.focus();
    }

    /// Marks the widget selected, so its painter draws a form-editor outline.
    pub fn set_selected(&self, selected: bool) {
        self.state.borrow_mut().selected = selected;
        self.control.set_selected(selected);
    }

    /// The whole text.
    pub fn text(&self) -> String {
        self.state.borrow().buffer.text()
    }

    /// Replaces the text, moving the caret to the start and clearing undo.
    pub fn set_text(&self, text: &str) {
        let mut state = self.state.borrow_mut();
        state.buffer = Buffer::new(text);
        state.view = View::new();
        state.reset_highlight();
        drop(state);
        self.control.invalidate();
    }

    /// Moves the caret to `line`/`col` (both zero-based, `col` in chars) and
    /// scrolls it into view.
    pub fn goto(&self, line: usize, col: usize) {
        let ui = self.control.ui().clone();
        {
            let mut state = self.state.borrow_mut();
            let line = line.min(state.buffer.line_count().saturating_sub(1));
            let start = state.buffer.line_start(line);
            let end = state.buffer.line_end(line);
            let position = start + col.min(end - start);
            state.view.caret = position;
            state.view.anchor = position;
            state.view.goal_col = None;
            events::ensure_visible(&mut state, &ui, self.control.id());
        }
        self.control.invalidate();
    }

    /// Replaces the diagnostic markers (squiggles, tints and breakpoints).
    pub fn set_markers(&self, markers: Vec<Marker>) {
        self.state.borrow_mut().markers = markers;
        self.control.invalidate();
    }

    /// The caret's char offset.
    pub fn caret(&self) -> usize {
        self.state.borrow().view.caret
    }

    /// The caret as a zero-based `(line, column)` pair, `column` in chars.
    pub fn caret_line_col(&self) -> (usize, usize) {
        let state = self.state.borrow();
        let caret = state.view.caret;
        let line = state.buffer.line_of_char(caret);
        (line, caret - state.buffer.line_start(line))
    }

    /// The buffer's revision counter: it moves on every text change and stays
    /// put otherwise, so a caller can tell whether a command changed the text
    /// without comparing it.
    pub fn revision(&self) -> u64 {
        self.state.borrow().buffer.revision()
    }

    /// Moves the caret to the char offset `offset`, clearing any selection and
    /// scrolling it into view.
    pub fn set_caret(&self, offset: usize) {
        let ui = self.control.ui().clone();
        {
            let mut state = self.state.borrow_mut();
            let offset = offset.min(state.buffer.len_chars());
            state.view.caret = offset;
            state.view.anchor = offset;
            state.view.goal_col = None;
            events::ensure_visible(&mut state, &ui, self.control.id());
        }
        self.control.invalidate();
    }

    /// Selects the char range `start..end` (ordered) and scrolls to it.
    pub fn select(&self, start: usize, end: usize) {
        let ui = self.control.ui().clone();
        {
            let mut state = self.state.borrow_mut();
            let (start, end) = (start.min(end), start.max(end));
            state.view.anchor = start.min(state.buffer.len_chars());
            state.view.caret = end.min(state.buffer.len_chars());
            state.view.goal_col = None;
            events::ensure_visible(&mut state, &ui, self.control.id());
        }
        self.control.invalidate();
    }

    /// Inserts `text` at the caret, replacing the selection. Returns whether the
    /// text changed.
    ///
    /// Unlike typing, this does not raise [`Editor::on_change`]; the caller owns
    /// the edit and is responsible for any dirty tracking.
    pub fn insert_text(&self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            edit::splice(&mut state.buffer, &mut state.view, text, false);
            state.sync_highlight();
        }
        self.control.invalidate();
        true
    }

    /// Replaces the char range `start..end` with `text`. Returns whether the
    /// text changed.
    pub fn replace(&self, start: usize, end: usize, text: &str) -> bool {
        let ui = self.control.ui().clone();
        {
            let mut state = self.state.borrow_mut();
            let len = state.buffer.len_chars();
            let (start, end) = (start.min(end).min(len), start.max(end).min(len));
            if start == end && text.is_empty() {
                return false;
            }
            state.buffer.replace(start..end, text, false);
            state.view.caret = start + text.chars().count();
            state.view.anchor = state.view.caret;
            state.view.goal_col = None;
            state.sync_highlight();
            events::ensure_visible(&mut state, &ui, self.control.id());
        }
        self.control.invalidate();
        true
    }

    /// Undoes the last edit, returning whether the text changed.
    ///
    /// The menu Edit → Undo action calls this; it does not raise
    /// [`Editor::on_change`], so the caller owns dirty tracking.
    pub fn undo(&self) -> bool {
        self.edit(|state| edit::undo(&mut state.buffer, &mut state.view))
    }

    /// Redoes the last undone edit, returning whether the text changed.
    pub fn redo(&self) -> bool {
        self.edit(|state| edit::redo(&mut state.buffer, &mut state.view))
    }

    /// Copies the selection (or the caret's line) to the clipboard, returning
    /// whether anything was copied. The text does not change.
    pub fn copy(&self) -> bool {
        let state = self.state.borrow();
        edit::copy(&state.buffer, &state.view, state.clipboard.as_ref())
    }

    /// Cuts the selection (or the caret's line) to the clipboard, returning
    /// whether the text changed.
    pub fn cut(&self) -> bool {
        self.edit(|state| edit::cut(&mut state.buffer, &mut state.view, state.clipboard.as_ref()))
    }

    /// Pastes the clipboard at the caret, replacing the selection. Returns
    /// whether the text changed.
    pub fn paste(&self) -> bool {
        self.edit(|state| edit::paste(&mut state.buffer, &mut state.view, state.clipboard.as_ref()))
    }

    /// Deletes the selection, returning whether the text changed. With no
    /// selection nothing is deleted, matching the Edit → Delete menu action.
    pub fn delete_selection(&self) -> bool {
        self.edit(|state| {
            let Some((start, end)) = state.view.selection() else {
                return false;
            };
            state.buffer.remove(start..end, false);
            state.view.caret = start;
            state.view.anchor = start;
            state.view.goal_col = None;
            true
        })
    }

    /// Selects the whole buffer.
    pub fn select_all(&self) {
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            state.view.select_all(&state.buffer);
        }
        self.control.invalidate();
    }

    /// Runs a text-changing command: re-lexes the affected lines, repaints and
    /// reports whether anything changed.
    fn edit(&self, command: impl FnOnce(&mut EditorState) -> bool) -> bool {
        let ui = self.control.ui().clone();
        let changed = {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let changed = command(state);
            if changed {
                state.sync_highlight();
                // Keep the caret on screen, as the keyboard path does after the
                // same edits (a paste or an undo can move it far away).
                events::ensure_visible(state, &ui, self.control.id());
            }
            changed
        };
        if changed {
            self.control.invalidate();
        }
        changed
    }

    /// Finds `query` relative to the caret, returning the matched char range.
    ///
    /// A forward search starts at the caret and wraps to the top; a backward
    /// search takes the last match before the caret and wraps to the bottom. An
    /// invalid regular expression is reported as an error message.
    pub fn find(
        &self,
        query: &find::Query,
        case_sensitive: bool,
        forward: bool,
    ) -> std::result::Result<Option<(usize, usize)>, String> {
        let state = self.state.borrow();
        let text = state.buffer.text();
        let found = find::matches(&text, query, case_sensitive)?;
        // Search from the ordered selection bounds, not the caret: the caret
        // sits at one end of the match just selected (the far end after a
        // forward find), so a backward find from it would pick that match again.
        let caret = state.view.caret;
        let (from, to) = state.view.selection().unwrap_or((caret, caret));
        let chosen = if forward {
            found
                .iter()
                .copied()
                .find(|(start, _)| *start >= to)
                .or_else(|| found.first().copied())
        } else {
            found
                .iter()
                .rev()
                .copied()
                .find(|(_, end)| *end <= from)
                .or_else(|| found.last().copied())
        };
        Ok(chosen)
    }

    /// Selects the next (or previous) match of `query`. Returns whether one was
    /// found; an invalid regular expression is reported as an error.
    pub fn find_next(
        &self,
        query: &find::Query,
        case_sensitive: bool,
        forward: bool,
    ) -> std::result::Result<bool, String> {
        match self.find(query, case_sensitive, forward)? {
            Some((start, end)) => {
                self.select(start, end);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Replaces the display options.
    pub fn set_options(&self, options: Options) {
        self.state.borrow_mut().options = options;
        self.control.invalidate();
    }

    /// Whether Undo has an edit to undo.
    pub fn can_undo(&self) -> bool {
        self.state.borrow().buffer.can_undo()
    }

    /// Whether Redo has an edit to redo.
    pub fn can_redo(&self) -> bool {
        self.state.borrow().buffer.can_redo()
    }

    /// Whether the clipboard holds text a paste would insert.
    pub fn can_paste(&self) -> bool {
        self.state
            .borrow()
            .clipboard
            .text()
            .is_some_and(|text| !text.is_empty())
    }

    /// Whether the buffer holds no text.
    pub fn is_empty(&self) -> bool {
        self.state.borrow().buffer.len_chars() == 0
    }

    /// The selected char range, ordered, or `None` when nothing is selected.
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.state.borrow().view.selection()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_default_clipboard_is_the_windows_portable_one() {
        use std::rc::Rc;

        use xui_canvas::OffscreenBackend;
        use xui_core::app::{App, Ui, run_app};
        use xui_core::backend::PlatformSpec;
        use xui_core::geometry::Rect;
        use xui_core::units::Dip;

        struct Empty;
        impl App for Empty {
            type Msg = ();
            fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
        }

        run_app(
            Rc::new(OffscreenBackend::new()),
            PlatformSpec::new("clipboard").size(Dip(200.0), Dip(100.0)),
            |ui| {
                let editor =
                    super::Editor::<()>::new(ui, Rect::new(0, 0, 200, 100)).expect("editor");
                editor.set_text("hello");
                editor.select_all();
                assert!(editor.copy());
                assert_eq!(ui.clipboard_text().as_deref(), Some("hello"));
                ui.set_clipboard_text("world");
                assert!(editor.paste());
                assert_eq!(editor.text(), "world");
                Empty
            },
        )
        .expect("run_app");
    }

    #[test]
    fn command_state_queries_follow_the_buffer_and_clipboard() {
        use std::rc::Rc;

        use xui_canvas::OffscreenBackend;
        use xui_core::app::{App, Ui, run_app};
        use xui_core::backend::PlatformSpec;
        use xui_core::geometry::Rect;
        use xui_core::units::Dip;

        use crate::Clipboard;
        use crate::platform::InProcessClipboard;

        struct Empty;
        impl App for Empty {
            type Msg = ();
            fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
        }

        run_app(
            Rc::new(OffscreenBackend::new()),
            PlatformSpec::new("queries").size(Dip(200.0), Dip(100.0)),
            |ui| {
                let editor =
                    super::Editor::<()>::new(ui, Rect::new(0, 0, 200, 100)).expect("editor");
                editor.set_clipboard(InProcessClipboard);
                InProcessClipboard.set_text("");
                assert!(editor.is_empty());
                assert!(!editor.can_undo() && !editor.can_redo() && !editor.can_paste());
                assert!(!editor.insert_text(""), "an empty insert is a no-op");
                assert!(!editor.can_undo());
                assert!(editor.insert_text("hi"));
                assert!(!editor.is_empty() && editor.can_undo());
                assert!(editor.undo());
                assert!(editor.can_redo());
                InProcessClipboard.set_text("x");
                assert!(editor.can_paste());
                Empty
            },
        )
        .expect("run_app");
    }

    #[test]
    fn with_clipboard_installs_the_given_clipboard() {
        use std::rc::Rc;

        use xui_canvas::OffscreenBackend;
        use xui_core::app::{App, Ui, run_app};
        use xui_core::backend::PlatformSpec;
        use xui_core::geometry::Rect;
        use xui_core::units::Dip;

        use crate::Clipboard;

        struct Host;
        impl Clipboard for Host {
            fn text(&self) -> Option<String> {
                Some("from the host".to_owned())
            }
            fn set_text(&self, _text: &str) {}
        }

        struct Empty;
        impl App for Empty {
            type Msg = ();
            fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
        }

        run_app(
            Rc::new(OffscreenBackend::new()),
            PlatformSpec::new("clipboard").size(Dip(200.0), Dip(100.0)),
            |ui| {
                let editor = super::Editor::<()>::new(ui, Rect::new(0, 0, 200, 100))
                    .expect("editor")
                    .with_clipboard(Host);
                assert_eq!(
                    editor.state.borrow().clipboard.text().as_deref(),
                    Some("from the host")
                );
                Empty
            },
        )
        .expect("run_app");
    }

    use crate::markers::{Marker, MarkerKind};
    use crate::options::Options;
    use crate::state::EditorState;

    #[test]
    fn default_options_are_monospace_friendly() {
        let options = Options::default();
        assert_eq!(options.tab_width, 4);
        assert!(options.show_gutter);
    }

    #[test]
    fn a_marker_can_be_attached_to_a_diagnostic_span() {
        let marker = Marker::new(4, 2, 9, MarkerKind::Error);
        assert!(marker.has_span());
    }

    #[test]
    fn the_state_exposes_selection_over_the_buffer() {
        let state = EditorState::new("abc\ndef", Options::default(), Box::new(SafeClipboard));
        assert_eq!(state.buffer.line_count(), 2);
    }

    #[test]
    fn the_widget_builds_and_paints_through_an_offscreen_backend() {
        use std::cell::RefCell;
        use std::rc::Rc;

        use xui_canvas::OffscreenBackend;
        use xui_core::backend::PlatformSpec;
        use xui_core::geometry::Rect;
        use xui_core::units::Dip;
        use xui_core::{App, Image, run_app};

        struct Empty;

        impl App for Empty {
            type Msg = ();
            fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
        }

        let backend = Rc::new(OffscreenBackend::new());
        let shot: Rc<RefCell<Option<Image>>> = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&shot);
        run_app(
            backend,
            PlatformSpec::new("editor").size(Dip(200.0), Dip(120.0)),
            move |ui| {
                let editor = crate::Editor::new(ui, Rect::new(0, 0, 200, 120)).expect("editor");
                editor.set_text("hello\nworld");
                *sink.borrow_mut() = ui.capture().ok();
                Empty
            },
        )
        .expect("run_app");

        let captured = shot.borrow();
        let image = captured.as_ref().expect("the editor painted something");
        assert!(image.width() > 0 && image.height() > 0);
    }

    /// A click lands on the line and column under it wherever the editor sits.
    ///
    /// Mouse events arrive in node-local coordinates, so hit-testing must not
    /// use the editor's rectangle relative to its parent: an editor offset
    /// inside a panel (a code tab below its header) would map every click to
    /// the wrong cell.
    #[test]
    fn a_click_inside_an_offset_editor_puts_the_caret_under_the_pointer() {
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;

        use xui_canvas::snapshot::{Snapshot, render_with};
        use xui_core::geometry::Rect;
        use xui_core::units::Dip;
        use xui_core::widget::Panel;
        use xui_core::{App, Color};

        use crate::metrics::{CELL_PROBE, Metrics};

        struct Empty(#[allow(dead_code)] Panel<()>);

        impl App for Empty {
            type Msg = ();
            fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
        }

        let text: String = (0..12)
            .map(|n| {
                format!(
                    "line {n:02} xxxxxxxxxxxxxxxx
"
                )
            })
            .collect();
        let (line, col) = (4, 7);
        // The editor sits at (60, 50) inside a panel that sits at (30, 20).
        let panel_origin = (30, 20);
        let editor_origin = (60, 50);

        let editor: Rc<RefCell<Option<crate::Editor<()>>>> = Rc::new(RefCell::new(None));
        let target = Rc::new(Cell::new((0, 0)));
        let expected = Rc::new(Cell::new(0));
        let result = Rc::new(Cell::new(usize::MAX));
        render_with(
            Snapshot::new(Dip(500.0), Dip(400.0)),
            {
                let editor = Rc::clone(&editor);
                let target = Rc::clone(&target);
                let expected = Rc::clone(&expected);
                let text = text.clone();
                move |ui| {
                    let panel =
                        Panel::new(ui, Rect::new(panel_origin.0, panel_origin.1, 480, 380))?;
                    let scoped = ui.with_parent(panel.id());
                    let widget = crate::Editor::new(
                        &scoped,
                        Rect::new(editor_origin.0, editor_origin.1, 400, 300),
                    )?;
                    widget.set_text(&text);

                    let options = crate::Options::default();
                    let style = options.font.style(Color::rgb(0, 0, 0));
                    let measured = ui.measure_text(CELL_PROBE, &style, ui.dpi());
                    let metrics = Metrics::new(
                        measured,
                        widget.state.borrow().buffer.line_count(),
                        options.show_gutter,
                        ui.dpi(),
                    );
                    // The middle of the cell, in window coordinates.
                    target.set((
                        panel_origin.0
                            + editor_origin.0
                            + metrics.gutter
                            + col as i32 * metrics.advance
                            + metrics.advance / 2,
                        panel_origin.1
                            + editor_origin.1
                            + line as i32 * metrics.line_height
                            + metrics.line_height / 2,
                    ));
                    expected.set(widget.state.borrow().buffer.line_start(line) + col);
                    *editor.borrow_mut() = Some(widget);
                    Ok(Empty(panel))
                }
            },
            {
                let editor = Rc::clone(&editor);
                let target = Rc::clone(&target);
                let result = Rc::clone(&result);
                move |stage| {
                    let (x, y) = target.get();
                    stage.click(x, y);
                    result.set(editor.borrow().as_ref().expect("editor").caret());
                }
            },
        )
        .expect("render");
        assert_eq!(result.get(), expected.get());
    }

    #[test]
    fn repeated_find_previous_walks_back_through_the_matches() {
        use std::rc::Rc;

        use xui_canvas::OffscreenBackend;
        use xui_core::backend::PlatformSpec;
        use xui_core::geometry::Rect;
        use xui_core::units::Dip;
        use xui_core::{App, run_app};

        use crate::find::Query;

        struct Empty;

        impl App for Empty {
            type Msg = ();
            fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
        }

        run_app(
            Rc::new(OffscreenBackend::new()),
            PlatformSpec::new("find").size(Dip(300.0), Dip(200.0)),
            |ui| {
                let editor = crate::Editor::new(ui, Rect::new(0, 0, 300, 200)).expect("editor");
                editor.set_text("ab ab ab");
                let query = Query::literal("ab");
                let mut seen = Vec::new();
                for _ in 0..4 {
                    assert!(editor.find_next(&query, true, false).expect("valid"));
                    seen.push(editor.selection().expect("selected"));
                }
                assert_eq!(seen, [(6, 8), (3, 5), (0, 2), (6, 8)]);
                let mut seen = Vec::new();
                for _ in 0..3 {
                    assert!(editor.find_next(&query, true, true).expect("valid"));
                    seen.push(editor.selection().expect("selected"));
                }
                assert_eq!(seen, [(0, 2), (3, 5), (6, 8)]);
                Empty
            },
        )
        .expect("run_app");
    }

    #[test]
    fn programmatic_edits_and_find_work_on_a_live_widget() {
        use std::cell::RefCell;
        use std::rc::Rc;

        use xui_canvas::OffscreenBackend;
        use xui_core::backend::PlatformSpec;
        use xui_core::geometry::Rect;
        use xui_core::units::Dip;
        use xui_core::{App, run_app};

        use crate::find::Query;

        struct Empty;

        impl App for Empty {
            type Msg = ();
            fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
        }

        let check = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&check);
        run_app(
            Rc::new(OffscreenBackend::new()),
            PlatformSpec::new("edits").size(Dip(300.0), Dip(200.0)),
            move |ui| {
                let editor = crate::Editor::new(ui, Rect::new(0, 0, 300, 200)).expect("editor");
                assert!(editor.insert_text("fn form_load() {\n}\n"));
                assert_eq!(editor.text(), "fn form_load() {\n}\n");

                let query = Query::literal("form_load");
                assert!(editor.find_next(&query, true, true).expect("valid query"));
                assert_eq!(editor.selection(), Some((3, 12)));
                assert!(editor.replace(3, 12, "form_resize"));
                assert_eq!(editor.text(), "fn form_resize() {\n}\n");

                editor.set_caret(0);
                assert_eq!(editor.caret(), 0);
                *sink.borrow_mut() = Some(());
                Empty
            },
        )
        .expect("run_app");
        assert!(check.borrow().is_some());
    }

    #[test]
    fn the_edit_menu_commands_change_and_restore_the_buffer() {
        use std::cell::RefCell;
        use std::rc::Rc;

        use xui_canvas::OffscreenBackend;
        use xui_core::backend::PlatformSpec;
        use xui_core::geometry::Rect;
        use xui_core::units::Dip;
        use xui_core::{App, run_app};

        struct Empty;
        impl App for Empty {
            type Msg = ();
            fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
        }

        let check = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&check);
        run_app(
            Rc::new(OffscreenBackend::new()),
            PlatformSpec::new("edit-menu").size(Dip(300.0), Dip(200.0)),
            move |ui| {
                let editor = crate::Editor::new(ui, Rect::new(0, 0, 300, 200))
                    .expect("editor")
                    // The thread-local clipboard, so the test neither needs an OS
                    // clipboard (headless CI) nor clobbers the developer's.
                    .with_clipboard(crate::platform::InProcessClipboard);
                editor.set_text("hello world");
                editor.select_all();
                assert!(editor.cut(), "cut removed the selection");
                assert_eq!(editor.text(), "");
                assert!(editor.paste(), "paste restored it");
                assert_eq!(editor.text(), "hello world");
                assert!(editor.undo(), "undo removed the paste");
                assert_eq!(editor.text(), "");
                assert!(editor.redo(), "redo re-applied the paste");
                assert_eq!(editor.text(), "hello world");

                // Copy keeps the text; delete_selection clears it.
                editor.select(0, 5);
                assert!(editor.copy(), "copy found a selection");
                assert!(editor.delete_selection(), "delete cleared the selection");
                assert_eq!(editor.text(), " world");
                *sink.borrow_mut() = Some(());
                Empty
            },
        )
        .expect("run_app");
        assert!(check.borrow().is_some());
    }

    #[cfg(feature = "rhai-syntax")]
    #[test]
    fn switching_the_highlighter_on_a_live_editor_relexes_the_whole_buffer() {
        use std::cell::RefCell;
        use std::rc::Rc;

        use xui_canvas::OffscreenBackend;
        use xui_core::backend::PlatformSpec;
        use xui_core::geometry::Rect;
        use xui_core::units::Dip;
        use xui_core::{App, Image, run_app};

        struct Empty;

        impl App for Empty {
            type Msg = ();
            fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
        }

        let shots: Rc<RefCell<Vec<Image>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&shots);
        run_app(
            Rc::new(OffscreenBackend::new()),
            PlatformSpec::new("switch").size(Dip(300.0), Dip(200.0)),
            move |ui| {
                let editor = crate::Editor::new(ui, Rect::new(0, 0, 300, 200)).expect("editor");
                editor.set_text("let x = 1;\nlet y = 2;\n");
                if let Ok(image) = ui.capture() {
                    sink.borrow_mut().push(image);
                }
                editor.set_highlighter(crate::RhaiHighlighter);
                if let Ok(image) = ui.capture() {
                    sink.borrow_mut().push(image);
                }
                Empty
            },
        )
        .expect("run_app");

        let shots = shots.borrow();
        assert_eq!(shots.len(), 2);
        assert_ne!(
            shots[0], shots[1],
            "the plain and Rhai renders differ, so the whole buffer was re-lexed"
        );
    }

    use crate::platform::Clipboard;

    struct SafeClipboard;

    impl Clipboard for SafeClipboard {
        fn text(&self) -> Option<String> {
            None
        }
        fn set_text(&self, _text: &str) {}
    }
}
