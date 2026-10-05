#![forbid(unsafe_code)]

//! The window layout and the state mirror a host or test can read.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{Handle, LayoutExt, build, column, status_bar};
use xui_core::backend::Result;
use xui_core::widget::StatusBar;

use super::palette::Palette;
use super::toolbar::{StripItem, ToolStrip};
use super::{Msg, PaintCanvas};
use crate::model::{Pixel, SIZES, Tool};

/// The tool, size and action cells, in order. Save/Open are included only when
/// the storage is available; Resize is last so the other indices are stable.
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
    items.push(StripItem::Resize);
    items
}

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

/// The window's widgets, created by [`mount`].
pub(super) struct Widgets {
    pub canvas: Rc<PaintCanvas>,
    pub toolbar: Rc<ToolStrip>,
    pub palette: Rc<Palette>,
    pub status: Rc<StatusBar<Msg>>,
}

/// Builds the window: the tool strip across the top, the canvas taking the
/// rest, the palette and the status bar along the bottom. The strip and the
/// palette wrap their cells, so their height follows the window's width. `io`
/// is whether the storage is available, which adds the Save/Open cells.
pub(super) fn mount(ui: &Ui<Msg>, io: bool) -> Result<Widgets> {
    let (canvas, toolbar, palette, status) =
        (Handle::new(), Handle::new(), Handle::new(), Handle::new());
    ui.root(column().children((
        build(move |ui| ToolStrip::new(ui, strip_items(io))).bind(&toolbar),
        build(PaintCanvas::new).bind(&canvas).fill(1),
        build(Palette::new).bind(&palette),
        status_bar(&["--", "320 x 240", "Pencil"]).bind(&status),
    )))?;
    Ok(Widgets {
        canvas: canvas.get(),
        toolbar: toolbar.get(),
        palette: palette.get(),
        status: status.get(),
    })
}
