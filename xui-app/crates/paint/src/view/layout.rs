#![forbid(unsafe_code)]

//! The app layout and the state mirror a host or test can read.

use xui_core::geometry::Rect;
use xui_core::units::Dip;

use super::palette;
use super::toolbar::{self, StripItem};
use crate::model::{Pixel, SIZES, Tool};

/// The tool, size and action cells, in order. Save/Open are included only when
/// the storage is available.
pub(super) fn strip_items(io: bool) -> Vec<StripItem> {
    let mut items: Vec<StripItem> = Tool::ALL.into_iter().map(StripItem::Tool).collect();
    items.extend(SIZES.into_iter().map(StripItem::Size));
    items.extend([
        StripItem::Undo,
        StripItem::Redo,
        StripItem::Clear,
        StripItem::New,
    ]);
    if io {
        items.extend([StripItem::Save, StripItem::Open]);
    }
    items
}

/// The design height of the status bar.
const STATUS_HEIGHT: Dip = Dip(24.0);

/// The palette has this many fixed colours.
const PALETTE_COUNT: usize = 16;

// xui gap: G12 — the runtime owns the app and there is no public handle to it
// after `render_with`, so a test reads this mirror instead.

/// A read-only mirror of the app's state, updated after every message.
///
/// A host embedding the library can pass one to [`PaintApp::build_observed`] to
/// drive, say, its own window title or tray, and a test uses it to read the
/// model without reaching into the widgets. Without one, the app works exactly
/// as before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observer {
    /// The active tool.
    pub tool: Tool,
    /// The brush diameter.
    pub size: u32,
    /// The primary colour.
    pub primary: Pixel,
    /// The secondary colour.
    pub secondary: Pixel,
    /// Whether undo is available.
    pub can_undo: bool,
    /// Whether redo is available.
    pub can_redo: bool,
    /// The cursor in canvas pixels, if it is over the canvas.
    pub cursor: Option<(i32, i32)>,
    /// Whether a drag is in progress.
    pub dragging: bool,
    /// The four status-bar parts.
    pub status: [String; 4],
}

impl Default for Observer {
    fn default() -> Observer {
        Observer {
            tool: Tool::Pencil,
            size: 1,
            primary: [0, 0, 0, 255],
            secondary: [255, 255, 255, 255],
            can_undo: false,
            can_redo: false,
            cursor: None,
            dragging: false,
            status: Default::default(),
        }
    }
}

/// The device-pixel rectangles the app lays its widgets out in, derived from
/// the window's client rect and DPI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The tool strip across the top.
    pub toolbar: Rect,
    /// The drawing canvas.
    pub canvas: Rect,
    /// The colour palette.
    pub palette: Rect,
    /// The status bar at the bottom.
    pub status: Rect,
}

/// Computes the app layout for a `client` rect at `dpi`. `io` is whether the
/// storage is available, which determines whether Save/Open cells take space.
pub fn layout(client: Rect, dpi: u32, io: bool) -> Layout {
    let tool_height = toolbar::preferred_height(strip_items(io).len(), client.width(), dpi);
    let palette_height = palette::preferred_height(PALETTE_COUNT, client.width(), dpi);
    let status_height = STATUS_HEIGHT.to_px(dpi).value().max(1);
    let canvas_bottom =
        (client.bottom - palette_height - status_height).max(client.top + tool_height + 1);
    let palette_top = canvas_bottom;
    let palette_bottom = palette_top + palette_height;
    let status_top = palette_bottom;
    Layout {
        toolbar: Rect::new(
            client.left,
            client.top,
            client.right,
            client.top + tool_height,
        ),
        canvas: Rect::new(
            client.left,
            client.top + tool_height,
            client.right,
            canvas_bottom,
        ),
        palette: Rect::new(client.left, palette_top, client.right, palette_bottom),
        status: Rect::new(
            client.left,
            status_top,
            client.right,
            status_top + status_height,
        ),
    }
}
