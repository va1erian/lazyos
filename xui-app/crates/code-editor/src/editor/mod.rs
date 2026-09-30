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

mod find;
mod navigation;
mod text;

#[cfg(test)]
mod tests;

use crate::events;
use crate::lexer::Highlighter;
use crate::options::Options;
use crate::paint;
use crate::platform;
use crate::platform::Clipboard;
use crate::state::{EditorState, Effect};
use crate::theme::EditorTheme;

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
}
