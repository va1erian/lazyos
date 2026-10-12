#![forbid(unsafe_code)]

//! The menu bar: File, Edit, View, Image, Layers, Adjustments and Help.
//!
//! The menus are a static table ([`MENUS`]) of rows, each a message and a label
//! with a `&` mnemonic. Ids are derived from the row's place in the table, so
//! [`message`] (the `MenuId` -> `Msg` map the bar uses) and [`init`]/[`sync`]
//! (which enable and disable rows) read the same data and cannot drift apart. A
//! row's shortcut is not written here: it is looked up in
//! [`BINDINGS`](super::shortcuts::BINDINGS) when the menu is built.
//!
//! A row whose feature does not exist yet carries a [`Command`] and starts
//! disabled ([`Command::available`]); xui's `Menu::set_enabled` dims it.

use xui_core::arrange::{Build, menu_bar};
use xui_core::widget::{Menu, MenuId, MenuScope};

use super::shortcuts::chord_for;
use super::{Command, Msg};

/// One row of a menu.
#[derive(Clone, Copy)]
enum Row {
    /// A command: its message and its label.
    Item(Msg, &'static str),
    /// A divider.
    Separator,
}

/// One top-level menu.
struct Top {
    title: &'static str,
    rows: &'static [Row],
}

const fn cmd(command: Command, label: &'static str) -> Row {
    Row::Item(Msg::Command(command), label)
}

/// Every menu, in bar order.
static MENUS: &[Top] = &[
    Top {
        title: "&File",
        rows: &[
            Row::Item(Msg::New, "&New"),
            Row::Item(Msg::NewTransparent, "New &Transparent"),
            Row::Item(Msg::Open, "&Open..."),
            Row::Separator,
            Row::Item(Msg::Save, "&Save"),
            Row::Item(Msg::SaveAs, "Save &As..."),
            Row::Separator,
            Row::Item(Msg::Quit, "&Quit"),
        ],
    },
    Top {
        title: "&Edit",
        rows: &[
            Row::Item(Msg::Undo, "&Undo"),
            Row::Item(Msg::Redo, "&Redo"),
            Row::Separator,
            cmd(Command::Cut, "Cu&t"),
            cmd(Command::Copy, "&Copy"),
            cmd(Command::CopyMerged, "Copy &Merged"),
            cmd(Command::Paste, "&Paste"),
            cmd(Command::PasteNewLayer, "Paste into New &Layer"),
            cmd(Command::PasteNewImage, "Paste into New &Image"),
            Row::Separator,
            cmd(Command::EraseSelection, "&Erase Selection"),
            cmd(Command::FillSelection, "&Fill Selection"),
            cmd(Command::InvertSelection, "In&vert Selection"),
            Row::Separator,
            cmd(Command::SelectAll, "Select &All"),
            cmd(Command::Deselect, "&Deselect All"),
        ],
    },
    Top {
        title: "&View",
        rows: &[
            cmd(Command::ZoomIn, "Zoom &In"),
            cmd(Command::ZoomOut, "Zoom &Out"),
            cmd(Command::ZoomToWindow, "Zoom to &Window"),
            cmd(Command::ActualSize, "&Actual Size"),
            Row::Separator,
            cmd(Command::PixelGrid, "&Pixel Grid"),
        ],
    },
    Top {
        title: "&Image",
        rows: &[
            cmd(Command::CropToSelection, "Crop &to Selection"),
            cmd(Command::ResizeImage, "&Resize..."),
            Row::Item(Msg::CanvasSizeAsk, "Canvas &Size..."),
            Row::Separator,
            cmd(Command::FlipHorizontal, "Flip &Horizontal"),
            cmd(Command::FlipVertical, "Flip &Vertical"),
            cmd(Command::Rotate90Cw, "Rotate 90 C&lockwise"),
            cmd(Command::Rotate90Ccw, "Rotate 90 Counter&clockwise"),
            cmd(Command::Rotate180, "Rotate 1&80"),
            Row::Separator,
            cmd(Command::Flatten, "&Flatten"),
            Row::Separator,
            Row::Item(Msg::Clear, "Clear Ima&ge"),
        ],
    },
    Top {
        title: "&Layers",
        rows: &[
            cmd(Command::AddLayer, "&Add New Layer"),
            cmd(Command::DeleteLayer, "&Delete Layer"),
            cmd(Command::DuplicateLayer, "Dup&licate Layer"),
            cmd(Command::MergeLayerDown, "&Merge Layer Down"),
            Row::Separator,
            cmd(Command::MoveLayerUp, "Move Layer &Up"),
            cmd(Command::MoveLayerDown, "Move Layer D&own"),
            Row::Separator,
            cmd(Command::FlipLayerHorizontal, "Flip Layer &Horizontal"),
            cmd(Command::FlipLayerVertical, "Flip Layer &Vertical"),
            Row::Separator,
            cmd(Command::LayerProperties, "Layer &Properties..."),
        ],
    },
    Top {
        title: "&Adjustments",
        rows: &[
            cmd(Command::InvertColors, "&Invert Colors"),
            cmd(Command::BlackAndWhite, "&Black and White"),
            cmd(Command::Sepia, "&Sepia"),
            Row::Separator,
            cmd(Command::BrightnessContrast, "Brightness / &Contrast..."),
            cmd(Command::HueSaturation, "&Hue / Saturation..."),
        ],
    },
    Top {
        title: "&Help",
        rows: &[Row::Item(Msg::About, "&About Paint")],
    },
];

/// The id of a top-level title.
const fn title_id(menu: usize) -> MenuId {
    MenuId::new(menu)
}

/// The id of row `row` of menu `menu`: a block of a hundred per menu, past the
/// title ids.
const fn row_id(menu: usize, row: usize) -> MenuId {
    MenuId::new(100 * (menu + 1) + row)
}

/// The label a row shows: its text, then its chord after a gap.
fn label(msg: Msg, text: &str) -> String {
    match chord_for(msg) {
        Some(chord) => format!("{text}    {}", chord.label()),
        None => text.to_string(),
    }
}

/// The menu bar. A chosen row raises its message; map it with [`message`].
pub(super) fn bar() -> Build<Menu<Msg>, Msg> {
    menu_bar(|bar: &mut MenuScope<'_>| {
        for (menu, top) in MENUS.iter().enumerate() {
            bar.submenu(title_id(menu), top.title, |scope| {
                for (index, row) in top.rows.iter().enumerate() {
                    match row {
                        Row::Item(msg, text) => {
                            scope.item(row_id(menu, index), &label(*msg, text));
                        }
                        Row::Separator => {
                            scope.separator();
                        }
                    }
                }
            });
        }
    })
    .on_select_with(message)
}

/// The message a chosen id raises: `None` for a title or an unknown id.
pub(super) fn message(id: MenuId) -> Option<Msg> {
    items().find_map(|(item, msg)| (item == id).then_some(msg))
}

/// The id of the row that raises `msg`.
pub(super) fn id_of(msg: Msg) -> Option<MenuId> {
    items().find_map(|(id, bound)| (bound == msg).then_some(id))
}

/// Every item row with its id.
fn items() -> impl Iterator<Item = (MenuId, Msg)> {
    MENUS.iter().enumerate().flat_map(|(menu, top)| {
        top.rows
            .iter()
            .enumerate()
            .filter_map(move |(index, row)| match row {
                Row::Item(msg, _) => Some((row_id(menu, index), *msg)),
                Row::Separator => None,
            })
    })
}

/// What decides whether a row is enabled besides its command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Enabled {
    /// Whether there is something to undo.
    pub can_undo: bool,
    /// Whether there is something to redo.
    pub can_redo: bool,
    /// Whether Open and Save have a storage behind them.
    pub io: bool,
    /// Whether there is a selection.
    pub selection: bool,
    /// Whether there is a picture to paste.
    pub can_paste: bool,
    /// How many layers the image has.
    pub layers: usize,
    /// The active layer's index.
    pub active: usize,
}

impl Default for Enabled {
    fn default() -> Enabled {
        Enabled {
            can_undo: false,
            can_redo: false,
            io: false,
            selection: false,
            can_paste: false,
            layers: 1,
            active: 0,
        }
    }
}

/// Whether the row for `msg` is enabled in `state`.
pub(super) fn is_enabled(msg: Msg, state: Enabled) -> bool {
    match msg {
        Msg::Command(command) => {
            command.available()
                && (!command.needs_selection() || state.selection)
                && (!command.needs_clipboard() || state.can_paste)
                && command.layer_ok(state.layers, state.active)
        }
        Msg::Undo => state.can_undo,
        Msg::Redo => state.can_redo,
        Msg::Open | Msg::Save | Msg::SaveAs => state.io,
        _ => true,
    }
}

/// Whether `command` is enabled for `model`'s layers and selection, for the
/// Layers panel's buttons (the menu reads the same rule through [`Enabled`]).
pub(super) fn command_enabled(command: Command, model: &crate::model::Model) -> bool {
    let doc = model.document();
    is_enabled(
        Msg::Command(command),
        Enabled {
            selection: model.selection().is_some(),
            can_paste: true,
            layers: doc.layers().len(),
            active: doc.active(),
            ..Enabled::default()
        },
    )
}

/// Sets every row's enabled state, once the menu is built.
pub(super) fn init(menu: &Menu<Msg>, state: Enabled) {
    for (id, msg) in items() {
        menu.set_enabled(id, is_enabled(msg, state));
    }
}

/// Updates the rows `state` can change.
pub(super) fn sync(menu: &Menu<Msg>, state: Enabled) {
    for (id, msg) in items() {
        let live = match msg {
            Msg::Undo | Msg::Redo => true,
            Msg::Command(command) => {
                command.needs_selection() || command.needs_clipboard() || command.needs_layers()
            }
            _ => false,
        };
        if live {
            menu.set_enabled(id, is_enabled(msg, state));
        }
    }
}

#[cfg(test)]
mod tests;
