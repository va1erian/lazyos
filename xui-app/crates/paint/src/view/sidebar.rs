#![forbid(unsafe_code)]

//! The right column under Colors: the Layers panel with its button strip, and
//! the collapsible History list.
//!
//! The strip is xui buttons (icon, tooltip, enabled state), and History is
//! xui's `ListView`, whose quiet [`select`](xui_core::widget::ListView::select)
//! highlights the current step without raising a click. The Layers rows
//! themselves are [`LayersPanel`], a custom node.

use std::cell::{Cell, RefCell};

use xui_core::app::Ui;
use xui_core::arrange::{Entry, Handle, Layout, LayoutExt, build, button, column, list, row};
use xui_core::backend::WidgetId;
use xui_core::icon::Lucide;
use xui_core::widget::{Button, ListView};

use super::layers::LayersPanel;
use super::palette::Palette;
use super::{Command, Msg};
use crate::model::Model;

/// The design height of the button strip.
const STRIP: i32 = 30;
/// The design height of the History header button.
const HEADER: i32 = 26;

/// What a strip button does, in strip order.
const BUTTONS: [(Command, Lucide, &str); 6] = [
    (Command::AddLayer, Lucide::Plus, "Add new layer"),
    (Command::DeleteLayer, Lucide::Trash2, "Delete layer"),
    (Command::DuplicateLayer, Lucide::Copy, "Duplicate layer"),
    (
        Command::MergeLayerDown,
        Lucide::Download,
        "Merge layer down",
    ),
    (Command::MoveLayerUp, Lucide::ChevronUp, "Move layer up"),
    (
        Command::MoveLayerDown,
        Lucide::ChevronDown,
        "Move layer down",
    ),
];

/// The probe names of [`BUTTONS`], in the same order.
pub(super) const BUTTON_NAMES: [&str; 6] = [
    "layer_add",
    "layer_delete",
    "layer_duplicate",
    "layer_merge",
    "layer_up",
    "layer_down",
];

/// The column's widgets, kept to update them from the model.
pub(super) struct Sidebar {
    pub(super) layers: Handle<LayersPanel>,
    buttons: [Handle<Button<Msg>>; 6],
    toggle: Handle<Button<Msg>>,
    history: Handle<ListView<Msg>>,
    /// Whether the History list is showing.
    open: Cell<bool>,
    /// The names the list was last filled with, so an unchanged history does
    /// not reset the list's scroll.
    shown: RefCell<Vec<String>>,
    /// The step the list last highlighted and scrolled to.
    current: Cell<Option<usize>>,
}

impl Sidebar {
    pub(super) fn new() -> Sidebar {
        Sidebar {
            layers: Handle::new(),
            buttons: std::array::from_fn(|_| Handle::new()),
            toggle: Handle::new(),
            history: Handle::new(),
            open: Cell::new(true),
            shown: RefCell::new(Vec::new()),
            current: Cell::new(None),
        }
    }

    /// The column: `palette` first, then Layers, its strip and History.
    pub(super) fn layout(&self, palette: &Handle<Palette>) -> Layout<Msg> {
        let strip = row().gap(2).children((
            strip_button(0, &self.buttons[0]),
            strip_button(1, &self.buttons[1]),
            strip_button(2, &self.buttons[2]),
            strip_button(3, &self.buttons[3]),
            strip_button(4, &self.buttons[4]),
            strip_button(5, &self.buttons[5]),
        ));
        column().gap(4).children((
            build(Palette::new).bind(palette),
            build(LayersPanel::new).bind(&self.layers).fill(3),
            strip.fixed(STRIP),
            button("History")
                .icon(Lucide::ChevronDown)
                .on_click(Msg::HistoryToggle)
                .bind(&self.toggle)
                .fixed(HEADER),
            list()
                .on_select(Msg::HistoryJump)
                .bind(&self.history)
                .fill(2),
        ))
    }

    /// Enables the strip by what the model allows, refreshes the layer rows and
    /// the history list.
    pub(super) fn sync(&self, ui: &Ui<Msg>, model: &Model) {
        let layers = self.layers.get();
        layers.sync(model);
        for (handle, (command, ..)) in self.buttons.iter().zip(BUTTONS) {
            handle
                .get()
                .set_enabled(super::menu::command_enabled(command, model));
        }
        self.sync_history(ui, model);
    }

    fn sync_history(&self, ui: &Ui<Msg>, model: &Model) {
        let history = model.history();
        let mut names = vec!["Start".to_string()];
        names.extend(history.undo_names().map(str::to_owned));
        names.extend(history.redo_names().map(str::to_owned));
        let list = self.history.get();
        let current = history.undo_len();
        let relisted = *self.shown.borrow() != names;
        if relisted {
            let rows: Vec<&str> = names.iter().map(String::as_str).collect();
            list.set_items(&rows);
            *self.shown.borrow_mut() = names;
        }
        // Quiet: highlighting the current step must not click it. The list is
        // scrolled to it only when the step or the rows changed, so a wheel
        // scroll through the list is not undone by the next message.
        let moved = self.current.replace(Some(current)) != Some(current);
        if relisted || moved {
            list.select(Some(current));
            list.ensure_visible(current);
        }
        ui.set_visible(list.id(), self.open.get());
    }

    /// Collapses or expands the History list.
    pub(super) fn toggle(&self, ui: &Ui<Msg>) {
        self.open.set(!self.open.get());
        let icon = if self.open.get() {
            Lucide::ChevronDown
        } else {
            Lucide::ChevronRight
        };
        self.toggle.get().set_icon(Some(icon));
        ui.set_visible(self.history.get().id(), self.open.get());
        ui.relayout();
    }

    /// Whether the History list is showing.
    pub(super) fn history_open(&self) -> bool {
        self.open.get()
    }

    /// The History list's node, for tests and the probe.
    pub(super) fn history_id(&self) -> WidgetId {
        self.history.get().id()
    }

    /// The History header button's node.
    pub(super) fn toggle_id(&self) -> WidgetId {
        self.toggle.get().id()
    }

    /// The strip button `index`'s node, with its probe name.
    pub(super) fn button_ids(&self) -> impl Iterator<Item = (&'static str, WidgetId)> + '_ {
        self.buttons
            .iter()
            .zip(BUTTON_NAMES)
            .map(|(handle, name)| (name, handle.get().id()))
    }
}

/// One strip button, bound to `handle`.
fn strip_button(index: usize, handle: &Handle<Button<Msg>>) -> Entry<Msg> {
    let (command, icon, tip) = BUTTONS[index];
    button("")
        .icon(icon)
        .tooltip(tip)
        .on_click(Msg::Command(command))
        .bind(handle)
        .fill(1)
}
