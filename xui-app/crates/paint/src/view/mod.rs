#![forbid(unsafe_code)]

//! The xui view: a canvas widget, a tool strip, a palette and the app wiring.

mod app;
mod canvas;
mod icons;
mod layout;
mod palette;
mod toolbar;

pub use app::PaintApp;
pub use canvas::{CanvasMsg, PaintCanvas};
pub use layout::{Layout, Observer, layout};
pub use palette::Palette;
pub use toolbar::{StripItem, ToolStrip};

use xui_core::Color;

use crate::model::Pixel;

// xui gap: G6 — no portable accelerator registration, so the app is mouse-only.
// xui gap: G11 — the event vocabulary has no touch/pen/pressure.
/// The message the widgets map input to; [`app::PaintApp::update`] turns it into
/// model calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    /// Select a tool.
    Tool(crate::model::Tool),
    /// Select a brush diameter.
    Size(u32),
    /// Pick a palette colour for a side.
    Palette {
        /// The chosen colour.
        color: Pixel,
        /// Which side it paints.
        side: crate::model::Side,
    },
    /// Exchange the primary and secondary colours.
    SwapColors,
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
    /// Load through the storage.
    Open,
    /// A canvas interaction.
    Canvas(CanvasMsg),
}

/// A colour as a portable xui [`Color`].
pub(crate) fn color_of(pixel: Pixel) -> Color {
    Color::rgb(pixel[0], pixel[1], pixel[2])
}
