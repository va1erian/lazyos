#![forbid(unsafe_code)]

//! The xui view: the menu bar, the tool options bar, a tool grid, a canvas, the
//! Colors panel and the app wiring.

mod app;
mod canvas;
mod coloredit;
mod command;
mod icons;
mod layerprops;
mod layers;
mod layout;
mod menu;
mod options;
mod palette;
pub mod shortcuts;
mod sidebar;
mod sysclip;
mod toolbar;

pub use app::{PaintApp, Setup};
pub use canvas::{CanvasMsg, PaintCanvas};
pub use command::Command;
pub use layers::LayersPanel;
pub use layout::{COLORS_WIDTH, Observer};
pub use options::{OptMsg, Shown, step_size, uses_size};
pub use palette::{COLORS, PALETTE, Palette};
pub use shortcuts::{ToolGroup, float_key, shortcut, typing_shortcut};
pub use sysclip::SystemClipboard;
pub use toolbar::ToolStrip;

use xui_core::Color;

use crate::model::Pixel;

// xui gap: G11 — the event vocabulary has no touch/pen/pressure.
/// The message the widgets map input to; [`app::PaintApp::update`] turns it into
/// model calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    /// Select a tool.
    Tool(crate::model::Tool),
    /// Set the brush, eraser and shape width in pixels (1 to 500).
    Size(u32),
    /// Change another option of the active tool.
    Opt(OptMsg),
    /// Pick a palette colour for a side.
    Palette {
        /// The chosen colour.
        color: Pixel,
        /// Which side it paints.
        side: crate::model::Side,
    },
    /// Exchange the primary and secondary colours.
    SwapColors,
    /// Restore the default colours: black primary, white secondary.
    ResetColors,
    /// Step the width along [`crate::model::SIZES`] (`[` and `]`).
    SizeStep(i32),
    /// Select the next tool of a group (a repeated tool key).
    CycleTool(ToolGroup),
    /// The pointer is over a tool of the grid, or left it (a status hint).
    HintTool(Option<crate::model::Tool>),
    /// Open the colour editor on a side's colour.
    EditColor(crate::model::Side),
    /// The editor changed the colour it is editing (opaque; the app keeps the
    /// colour's alpha).
    ColorEditing(Pixel),
    /// Keep the edited colour and close the editor.
    ColorDone,
    /// Restore the colour the edit began with and close the editor.
    ColorCancel,
    /// Undo the last action.
    Undo,
    /// Redo the last undone action.
    Redo,
    /// Clear the canvas.
    Clear,
    /// Start a new document.
    New,
    /// Save through the storage.
    Save,
    /// Save under a new name (the Save As picker).
    SaveAs,
    /// Quit the application.
    Quit,
    /// Show the About box.
    About,
    /// Move the floating pixels or the selection outline by this many pixels
    /// (the arrow keys while a move tool is active).
    Nudge(i32, i32),
    /// Land the floating pixels (Enter).
    FloatCommit,
    /// Drop the floating pixels where they were lifted from (Esc).
    FloatCancel,
    /// Choose how a new selection joins the old one.
    SelectMode(crate::model::CombineMode),
    /// Set the Magic Wand's tolerance in percent.
    WandTolerance(u8),
    /// Choose between a contiguous region and every similar pixel.
    WandContiguous(bool),
    /// The pasted picture is larger than the canvas: grow the canvas to it.
    PasteExpand,
    /// The pasted picture is larger than the canvas: keep the canvas size.
    PasteKeep,
    /// Make this layer (by index, 0 is the bottom) the one tools draw on.
    LayerSelect(usize),
    /// Toggle this layer's visibility (its eye in the Layers panel).
    LayerEye(usize),
    /// Open Layer Properties on this layer (a double click on its row).
    LayerProps(usize),
    /// The Layer Properties controls changed; the app reads them back.
    PropsChanged,
    /// Keep the Layer Properties edit.
    PropsOk,
    /// Restore the layer's properties and close Layer Properties.
    PropsCancel,
    /// Collapse or expand the History list.
    HistoryToggle,
    /// A History row was clicked: go to the state after that many edits (row 0
    /// is the start).
    HistoryJump(usize),
    /// The colour editor's alpha slider moved (0-255).
    ColorAlpha(u8),
    /// Start a new document with a transparent background.
    NewTransparent,
    /// A command whose feature a later phase builds; see [`Command`].
    Command(Command),
    /// Print the named widget rectangles (UI probe images only).
    Probe,
    /// Load through the storage.
    Open,
    /// Ask for a new canvas size (opens the prompt).
    ResizeAsk,
    /// The resize prompt was accepted; the text is in the app's slot.
    ResizeChosen,
    /// Load the start-up file through the storage, without a dialog.
    OpenStartup,
    /// The Open picker returned a path (held in the app's slot).
    OpenChosen,
    /// The Save As picker returned a path (held in the app's slot).
    SaveChosen,
    /// A dialog was dismissed without a choice; the canvas takes the focus back.
    DialogClosed,
    /// A canvas interaction.
    Canvas(CanvasMsg),
}

/// The menu row that raises `msg`, for a host or test that reads the menu bar's
/// state ([`PaintApp::menu`]).
pub fn menu_item(msg: Msg) -> Option<xui_core::widget::MenuId> {
    menu::id_of(msg)
}

/// A colour as a portable xui [`Color`].
pub(crate) fn color_of(pixel: Pixel) -> Color {
    Color::rgb(pixel[0], pixel[1], pixel[2])
}
