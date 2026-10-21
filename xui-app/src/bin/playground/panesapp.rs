//! The dividers and the bottom row's switch, on the running window
//! ([`crate::panes`] holds the numbers and the file).
//!
//! The Explorer | right column divider is an xui `Split` at a fixed design
//! width, so a bigger window gives the right column the difference. The doc
//! pane | bottom row divider is a stacked `Split` too, but the app sets its
//! position from the saved share whenever the window is resized, so the doc
//! pane and the editor/Output both grow in proportion. Hiding the bottom row
//! hides its panel (nothing in it takes input or the focus) and gives the doc
//! pane the whole column; the divider stays at the bottom edge, and pulling it
//! up shows the row again.

use std::path::PathBuf;

use xui_app::trace::{self, Channel};
use xui_core::app::Ui;
use xui_core::backend::WidgetId;
use xui_core::geometry::Rect;
use xui_core::{Dip, Px};

use crate::app::Playground;
use crate::menus::VIEW_BOTTOM;
use crate::msg::Msg;
use crate::panes::{self, Saved};
use crate::ui::Widgets;

/// The folder under `$HOME/.apps` the layout file lives in.
const SYSTEM_NAME: &str = crate::notice::SYSTEM_NAME;
/// A position no column is tall enough for: "everything to the first pane".
const ALL: Dip = Dip(1.0e5);

/// The layout state of the main window.
pub struct PaneState {
    pub saved: Saved,
    /// Where it is stored (`None`: not remembered, as in a test).
    file: Option<PathBuf>,
    /// The saved values changed since they were written.
    dirty: bool,
    /// Something wants the bottom row shown (a run, an insert, a loaded
    /// script): `Some(true)` when the editor too should take the keyboard.
    reveal: Option<bool>,
}

impl PaneState {
    /// The state a stored layout gives: the file of the user whose home the
    /// folder pickers use.
    pub fn stored() -> PaneState {
        let home = xui_app::platform::dirs::default_dir();
        let file = panes::path(&home, SYSTEM_NAME);
        let exists = file.is_file();
        let saved = panes::load(&file);
        if exists {
            trace::line!(
                Channel::Playground,
                "LAYOUT:LOADED:{}:{:.2}:{}",
                saved.explorer as i32,
                saved.doc_share,
                if saved.bottom_visible {
                    "shown"
                } else {
                    "hidden"
                }
            );
        }
        PaneState {
            saved,
            file: Some(file),
            dirty: false,
            reveal: None,
        }
    }

    /// Write the layout when it changed. A failure is dropped: the layout is
    /// then kept for this run only.
    fn write_if_dirty(&mut self) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        if let Some(file) = &self.file {
            let _ = panes::store(file, &self.saved);
        }
        trace::line!(
            Channel::Playground,
            "LAYOUT:SAVED:{}:{:.2}",
            self.saved.explorer as i32,
            self.saved.doc_share
        );
    }
}

/// Set the doc pane | bottom row divider for `saved` in the window `w` lives in.
pub fn fit(ui: &Ui<Msg>, w: &Widgets, saved: &Saved) {
    let split = w.split_doc.get();
    let bounds = ui.bounds(split.id());
    if bounds.is_empty() {
        return;
    }
    let dpi = ui.dpi();
    if saved.bottom_visible {
        split.set_min(Dip(panes::DOC_MIN), Dip(panes::BOTTOM_MIN));
        let doc = panes::doc_height(bounds.height(), saved.doc_share, dpi as f32 / 96.0);
        split.set_position(Px(doc).to_dip(dpi));
    } else {
        split.set_min(Dip(panes::DOC_MIN), Dip(0.0));
        split.set_position(ALL);
    }
    ui.set_visible(w.bottom.get().id(), saved.bottom_visible);
}

impl Playground {
    /// Whether the editor and the Output show.
    pub fn bottom_shown(&self) -> bool {
        self.panes.saved.bottom_visible
    }

    /// Size the doc pane | bottom row divider to the window and the saved
    /// share (the window was resized, or the row was shown or hidden).
    pub fn fit_panes(&mut self, ui: &mut Ui<Msg>) {
        fit(ui, &self.w, &self.panes.saved);
        self.sync_toggle();
    }

    /// The switch's two faces (the menu check and the button's text).
    fn sync_toggle(&self) {
        let shown = self.bottom_shown();
        self.w.menu.get().set_checked(VIEW_BOTTOM, shown);
        let text = if shown { "Hide editor" } else { "Show editor" };
        xui_core::widget::HasText::set_text(&*self.w.toggle_bottom.get(), text);
    }

    /// Ctrl+J, the button, or the View menu's check: flip the row.
    pub fn toggle_bottom(&mut self, ui: &mut Ui<Msg>) {
        self.set_bottom(ui, !self.bottom_shown());
    }

    /// Show or hide the editor and the Output. Nothing runs because of it,
    /// and the runs and the Output keep going while it is hidden.
    pub fn set_bottom(&mut self, ui: &mut Ui<Msg>, shown: bool) {
        if shown == self.bottom_shown() {
            self.sync_toggle();
            return;
        }
        // Keys typed into a hidden editor would go nowhere anyone can see.
        let editor = self.w.editor.get().0.id();
        let had_focus = ui.focus_holder() == Some(editor);
        self.panes.saved.bottom_visible = shown;
        self.panes.dirty = true;
        self.fit_panes(ui);
        if shown && had_focus {
            self.editor.focus();
        } else if !shown {
            self.focus_reading(ui);
        }
        self.panes.write_if_dirty();
        self.probe_soon();
        trace::line!(
            Channel::Playground,
            "LAYOUT:BOTTOM:{}",
            if shown { "SHOWN" } else { "HIDDEN" }
        );
    }

    /// Put the keyboard on the page being read (arrow keys and PageUp/PageDown scroll it).
    fn focus_reading(&self, ui: &Ui<Msg>) {
        let target = self
            .w
            .doc_scroll
            .try_get()
            .map(|view| view.id())
            .unwrap_or_else(|| self.w.search.get().id());
        ui.focus(target);
    }

    /// The dividers' rectangles for the UI probe: `split_main` (Explorer |
    /// right column) and `split_doc` (doc pane | bottom row), where a session
    /// presses to drag them.
    pub fn probe_dividers(
        &self,
        say: &dyn Fn(&str, Rect),
        abs: &dyn Fn(WidgetId) -> Rect,
        dpi: u32,
    ) {
        let thick = Dip(panes::DIVIDER).to_px(dpi).value();
        let main = self.w.split_main.get();
        let at = main.position().to_px(dpi).value();
        let r = abs(main.id());
        say(
            "split_main",
            Rect::new(r.left + at, r.top, r.left + at + thick, r.bottom),
        );
        let doc = self.w.split_doc.get();
        let at = doc.position().to_px(dpi).value();
        let r = abs(doc.id());
        say(
            "split_doc",
            Rect::new(r.left, r.top + at, r.right, r.top + at + thick),
        );
    }

    /// The Explorer divider moved to `width`.
    pub fn explorer_moved(&mut self, width: Dip) {
        let saved = Saved {
            explorer: width.0,
            ..self.panes.saved
        }
        .clamped();
        if saved != self.panes.saved {
            self.panes.saved = saved;
            self.panes.dirty = true;
        }
    }

    /// The doc pane | bottom row divider moved: the doc pane is now `doc` tall.
    /// Pulling a hidden row's divider up shows the row.
    pub fn doc_split_moved(&mut self, ui: &mut Ui<Msg>, doc: Dip) {
        let dpi = ui.dpi();
        let scale = dpi as f32 / 96.0;
        let total = ui.bounds(self.w.split_doc.get().id()).height();
        let doc_px = doc.to_px(dpi).value();
        if !self.bottom_shown() {
            let pulled = (total - doc_px) as f32 / scale;
            if pulled >= panes::REVEAL_PULL {
                self.panes.saved.doc_share = panes::share_of(doc_px, total, scale);
                self.set_bottom(ui, true);
            }
            return;
        }
        let share = panes::share_of(doc_px, total, scale);
        if (share - self.panes.saved.doc_share).abs() > f32::EPSILON {
            self.panes.saved.doc_share = share;
            self.panes.dirty = true;
        }
    }

    /// Something needs the editor or the Output on screen: the next
    /// [`apply_reveal`](Self::apply_reveal) shows the row, and gives the
    /// editor the keyboard when `editor` (text was put in it).
    pub fn reveal_bottom(&mut self, editor: bool) {
        let wants_editor = editor || self.panes.reveal == Some(true);
        self.panes.reveal = Some(wants_editor);
    }

    /// Show the row when something asked for it (end of every update).
    pub fn apply_reveal(&mut self, ui: &mut Ui<Msg>) {
        let Some(editor) = self.panes.reveal.take() else {
            return;
        };
        if !self.bottom_shown() {
            self.set_bottom(ui, true);
            if editor {
                self.editor.focus();
            }
        }
    }

    /// The editor takes the keyboard, when it is on screen.
    pub fn focus_editor(&self) {
        if self.bottom_shown() {
            self.editor.focus();
        }
    }

    /// The periodic write of a dragged layout (a drag changes it many times a
    /// second; the file is written once it settles).
    pub fn save_panes(&mut self) {
        self.panes.write_if_dirty();
    }
}
