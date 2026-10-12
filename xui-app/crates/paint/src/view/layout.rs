#![forbid(unsafe_code)]

//! The window layout and the state mirror a host or test can read.
//!
//! ```text
//! +--------------------------------------------------------+
//! | File Edit View Image Layers Adjustments Help           |  menu bar
//! | Pencil   Size: [4 px]                                  |  tool options bar
//! +----+----------------------------------+----------------+
//! |tool|                                  | Colors         |
//! |grid|  canvas (or the colour editor)   | primary/second |
//! | 2  |                                  | 32 swatches    |
//! |    |                                  | Layers + strip |
//! |    |                                  | History        |
//! +----+----------------------------------+----------------+
//! | 100, 40 | 320 x 240 | Pencil | message                 |  status bar
//! +--------------------------------------------------------+
//! ```
//!
//! The right column holds the Colors panel, the Layers panel with its button
//! strip and the collapsible History list ([`super::sidebar`]).

use std::rc::Rc;

use xui_core::Dip;
use xui_core::app::Ui;
use xui_core::arrange::{Handle, LayoutExt, build, column, row, stack, status_bar};
use xui_core::backend::Result;
use xui_core::widget::{Menu, StatusBar};

use super::coloredit::ColorEditor;
use super::layerprops::LayerPropsDialog;
use super::menu;
use super::options::{self, Options};
use super::palette::Palette;
use super::sidebar::Sidebar;
use super::toolbar::{self, ToolStrip};
use super::{Msg, PaintCanvas};
use crate::model::{Pixel, Side, Tool};

/// The menu bar's design height.
const MENU_HEIGHT: Dip = Dip(30.0);
/// The options bar's design height.
const OPTIONS_HEIGHT: Dip = Dip(36.0);
/// The right column's design width: eight 18 px swatches and their margins.
pub const COLORS_WIDTH: Dip = Dip(160.0);

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
    /// The zoom in whole percent.
    pub zoom_percent: u32,
    /// How far the picture is scrolled, in view pixels.
    pub view_origin: (i32, i32),
    /// The colour the editor is changing, while it is open.
    pub editing: Option<Side>,
    /// The selection's `(x, y, width, height)`, if there is one.
    pub selection: Option<(i32, i32, u32, u32)>,
    /// Whether lifted or pasted pixels are waiting to be placed.
    pub floating: bool,
    /// Whether a text block is being edited.
    pub text_editing: bool,
    /// Each layer's name, visibility and opacity, bottom layer first.
    pub layers: Vec<(String, bool, u8)>,
    /// The active layer's index.
    pub active_layer: usize,
    /// How many edits are applied, and how many in all (applied plus undone).
    pub history: (usize, usize),
    /// Whether Layer Properties is open.
    pub props_open: bool,
    /// The five status-bar parts: position, size, tool, message, zoom.
    pub status: [String; 5],
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
            zoom_percent: 100,
            view_origin: (0, 0),
            editing: None,
            selection: None,
            floating: false,
            text_editing: false,
            layers: Vec::new(),
            active_layer: 0,
            history: (0, 0),
            props_open: false,
            status: Default::default(),
        }
    }
}

/// The window's widgets, created by [`mount`].
pub(super) struct Widgets {
    pub menu: Rc<Menu<Msg>>,
    pub options: Options,
    pub canvas: Rc<PaintCanvas>,
    pub toolbar: Rc<ToolStrip>,
    pub palette: Rc<Palette>,
    pub editor: ColorEditor,
    pub props: LayerPropsDialog,
    pub sidebar: Sidebar,
    pub status: Rc<StatusBar<Msg>>,
}

/// Builds the window: the menu bar and the options bar across the top, the tool
/// grid, the canvas (with the colour editor stacked over it) and the Colors
/// panel in the middle, the status bar along the bottom.
pub(super) fn mount(ui: &Ui<Msg>, families: Vec<String>) -> Result<Widgets> {
    let (menu_handle, canvas, toolbar, palette, status) = (
        Handle::new(),
        Handle::new(),
        Handle::new(),
        Handle::new(),
        Handle::new(),
    );
    let options = Options::new(families);
    let editor = ColorEditor::new();
    let props = LayerPropsDialog::new();
    let sidebar = Sidebar::new();
    ui.root(
        column().children((
            menu::bar().bind(&menu_handle).height(MENU_HEIGHT),
            options::row(&options).fixed(OPTIONS_HEIGHT),
            row()
                .children((
                    build(ToolStrip::new).bind(&toolbar).width(toolbar::WIDTH),
                    stack()
                        .children((
                            build(PaintCanvas::new).bind(&canvas),
                            editor.layout(),
                            props.layout(),
                        ))
                        .fill(1),
                    sidebar.layout(&palette).width(COLORS_WIDTH),
                ))
                .fill(1),
            status_bar(&["--", "320 x 240", "Pencil", "", "100%"]).bind(&status),
        )),
    )?;
    editor.hide_initially(ui);
    props.hide_initially(ui);
    Ok(Widgets {
        menu: menu_handle.get(),
        options,
        canvas: canvas.get(),
        toolbar: toolbar.get(),
        palette: palette.get(),
        editor,
        props,
        sidebar,
        status: status.get(),
    })
}
