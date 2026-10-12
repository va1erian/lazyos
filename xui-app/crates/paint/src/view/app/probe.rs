#![forbid(unsafe_code)]

//! The UI probe hook: where the named controls are, for session scripts.
//!
//! The portable crate cannot print `UI:WIDGET` lines (that is the LazyOS
//! binary's `probe` module), so a host installs a hook with
//! [`PaintApp::set_probe`] and raises [`Msg::Probe`] once the window is laid
//! out. The hook gets the window's title (the line's `window=`), a name and a
//! window-relative rectangle.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::geometry::Rect;

use super::{Msg, PaintApp};
use crate::model::{Side, Tool};

/// The probe hook: `(window title, name, rectangle)`.
pub(super) type Hook = Rc<dyn Fn(&str, &str, Rect)>;

impl PaintApp {
    /// Installs the hook [`Msg::Probe`] reports through.
    pub fn set_probe(&mut self, hook: impl Fn(&str, &str, Rect) + 'static) {
        self.probe = Some(Rc::new(hook));
    }

    /// Reports every named control through the hook, if one is installed. The
    /// names: `canvas`, `menubar`, `tool:<Label>`, `size`, `swatch:primary`,
    /// `swatch:secondary`, `swap_colors`, `reset_colors`, `palette:<index>`, and
    /// `color_ok`/`color_cancel`/`color_alpha` while the colour editor is open,
    /// `layers`, `layer:<n>`, `layer_eye:<n>`, `layer_add`, `layer_delete`,
    /// `layer_duplicate`, `layer_merge`, `layer_up`, `layer_down`,
    /// `history_toggle`, `history`, and `props_*` while Layer Properties is open.
    pub(super) fn probe(&self, ui: &Ui<Msg>) {
        let Some(hook) = &self.probe else { return };
        let title = self.title();
        let report = |name: &str, rect: Rect| hook(&title, name, rect);
        report("canvas", ui.bounds(self.canvas.id()));
        if let Some(id) = self.menu.id() {
            report("menubar", ui.bounds(id));
        }
        report("size", ui.bounds(self.options.size_id()));
        report("sel_mode", ui.bounds(self.options.mode_id()));
        report("wand_tolerance", ui.bounds(self.options.tolerance_id()));
        for tool in Tool::ALL {
            if let Some(rect) = self.toolbar.tool_rect(tool) {
                report(&format!("tool:{}", tool.label()), rect);
            }
        }
        for (name, side) in [("primary", Side::Primary), ("secondary", Side::Secondary)] {
            report(&format!("swatch:{name}"), self.palette.swatch_rect(side));
        }
        report("swap_colors", self.palette.swap_rect());
        report("reset_colors", self.palette.reset_rect());
        if self.editing.is_some()
            && let Some((ok, cancel)) = self.editor.button_ids()
        {
            report("color_ok", ui.bounds(ok));
            report("color_cancel", ui.bounds(cancel));
        }
        report("layers", ui.bounds(self.sidebar.layers.get().id()));
        let panel = self.sidebar.layers.get();
        for layer in 0..panel.len() {
            if let Some(rect) = panel.row_rect(layer) {
                report(&format!("layer:{layer}"), rect);
            }
            if let Some(rect) = panel.eye_rect(layer) {
                report(&format!("layer_eye:{layer}"), rect);
            }
        }
        for (name, id) in self.sidebar.button_ids() {
            report(name, ui.bounds(id));
        }
        report("history_toggle", ui.bounds(self.sidebar.toggle_id()));
        if self.sidebar.history_open() {
            report("history", ui.bounds(self.sidebar.history_id()));
        }
        if self.editing_layer.is_some()
            && let Some(ids) = self.props.ids()
        {
            for (name, id) in ids {
                report(name, ui.bounds(id));
            }
        }
        if self.editing.is_some()
            && let Some(id) = self.editor.alpha_id()
        {
            report("color_alpha", ui.bounds(id));
        }
        for index in 0..crate::view::COLORS {
            if let Some(rect) = self.palette.color_rect(index) {
                report(&format!("palette:{index}"), rect);
            }
        }
    }
}
