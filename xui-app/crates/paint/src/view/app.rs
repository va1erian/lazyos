#![forbid(unsafe_code)]

//! The application: widget construction, layout from the client rect, and the
//! `Msg` -> model glue.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::widget::StatusBar;

use super::canvas::CanvasMsg;
use super::layout::{Observer, layout, strip_items};
use super::palette::Palette;
use super::toolbar::ToolStrip;
use super::{Msg, PaintCanvas};
use crate::model::{Model, Side};
use crate::storage::Storage;

use crate::{DEFAULT_HEIGHT, DEFAULT_WIDTH};

/// The paint application.
pub struct PaintApp {
    model: Model,
    canvas: PaintCanvas,
    toolbar: ToolStrip,
    palette: Palette,
    status: StatusBar<Msg>,
    storage: Rc<dyn Storage>,
    observer: Rc<RefCell<Observer>>,
    cursor: Option<(i32, i32)>,
    message: Option<String>,
}

impl PaintApp {
    /// Builds the widgets for `ui` and wires them to a fresh model.
    pub fn build(ui: &mut Ui<Msg>, storage: Rc<dyn Storage>) -> Result<PaintApp> {
        PaintApp::build_observed(ui, storage, Rc::new(RefCell::new(Observer::default())))
    }

    /// Like [`PaintApp::build`], additionally reporting state to `observer`.
    pub fn build_observed(
        ui: &mut Ui<Msg>,
        storage: Rc<dyn Storage>,
        observer: Rc<RefCell<Observer>>,
    ) -> Result<PaintApp> {
        let io = storage.available();
        let areas = layout(ui.client_rect(), ui.dpi(), io);
        let canvas = PaintCanvas::new(ui, areas.canvas)?;
        let toolbar = ToolStrip::new(ui, areas.toolbar, strip_items(io))?;
        let palette = Palette::new(ui, areas.palette)?;
        let status = StatusBar::new(ui, areas.status, &["--", "320 x 240", "Pencil"])?;

        let mut app = PaintApp {
            model: Model::new(DEFAULT_WIDTH, DEFAULT_HEIGHT),
            canvas,
            toolbar,
            palette,
            status,
            storage,
            observer,
            cursor: None,
            message: None,
        };
        app.sync();
        Ok(app)
    }

    /// The drawing model.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The canvas widget.
    pub fn canvas(&self) -> &PaintCanvas {
        &self.canvas
    }

    /// The tool strip.
    pub fn toolbar(&self) -> &ToolStrip {
        &self.toolbar
    }

    /// The palette.
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// The last save/load message, if any.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Rebuilds the painter state, toolbar and status bar from the model.
    fn sync(&mut self) {
        self.canvas.sync(
            self.model.bitmap(),
            self.model.revision(),
            self.model.preview(),
            self.model.size(),
        );
        self.toolbar.sync(&self.model, self.storage.available());
        self.palette
            .sync(self.model.primary(), self.model.secondary());
        self.refresh_status();
    }

    /// Rewrites the status bar parts.
    fn refresh_status(&mut self) {
        let position = match self.cursor {
            Some((x, y)) => format!("{x}, {y}"),
            None => "--".to_string(),
        };
        let (width, height) = self.model.bitmap().size();
        let size = format!("{width} x {height}");
        let tool = self.model.tool().label().to_string();
        let message = self.message.clone().unwrap_or_default();
        self.status.set_text(0, &position);
        self.status.set_text(1, &size);
        self.status.set_text(2, &tool);
        self.status.set_text(3, &message);

        let mut observer = self.observer.borrow_mut();
        observer.tool = self.model.tool();
        observer.size = self.model.size();
        observer.primary = self.model.primary();
        observer.secondary = self.model.secondary();
        observer.can_undo = self.model.history().can_undo();
        observer.can_redo = self.model.history().can_redo();
        observer.cursor = self.cursor;
        observer.dragging = self.model.is_dragging();
        observer.status = [position, size, tool, message];
    }

    /// Saves the bitmap through the storage, reporting the outcome.
    fn save(&mut self) {
        let result = self
            .model
            .bitmap()
            .encode_png()
            .map_err(|error| error.to_string())
            .and_then(|bytes| self.storage.save(&bytes));
        self.message = Some(match result {
            Ok(()) => "Saved".to_string(),
            Err(error) => format!("Save failed: {error}"),
        });
    }

    /// Decodes first, then swaps, so a failed load leaves the canvas intact.
    fn open(&mut self) {
        let Some(bytes) = self.storage.load() else {
            self.message = Some("Open failed: nothing stored".to_string());
            return;
        };
        match crate::model::Bitmap::decode(&bytes) {
            Ok(bitmap) => {
                self.model.load(bitmap);
                self.cursor = None;
                self.message = Some("Opened".to_string());
            }
            Err(error) => self.message = Some(format!("Open failed: {error}")),
        }
    }
}

impl App for PaintApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, _ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tool(tool) => self.model.set_tool(tool),
            Msg::Size(size) => self.model.set_size(size),
            Msg::Palette { color, side } => match side {
                Side::Primary => self.model.set_primary(color),
                Side::Secondary => self.model.set_secondary(color),
            },
            Msg::SwapColors => self.model.swap_colors(),
            Msg::Undo => self.model.undo(),
            Msg::Redo => self.model.redo(),
            Msg::Clear => self.model.clear(),
            Msg::New => {
                self.model.reset(DEFAULT_WIDTH, DEFAULT_HEIGHT);
                self.cursor = None;
            }
            Msg::Save => self.save(),
            Msg::Open => self.open(),
            Msg::Canvas(CanvasMsg::Down { x, y, side }) => {
                if self.model.is_dragging() {
                    self.model.end();
                }
                self.cursor = Some((x, y));
                self.model.begin(x, y, side);
            }
            Msg::Canvas(CanvasMsg::Move { x, y }) => {
                self.cursor = Some((x, y));
                self.model.extend(x, y);
            }
            Msg::Canvas(CanvasMsg::Up { x, y }) => {
                self.cursor = Some((x, y));
                self.model.extend(x, y);
                self.model.end();
            }
            Msg::Canvas(CanvasMsg::Cancel) => {
                self.model.cancel();
                self.cursor = None;
            }
        }
        self.sync();
    }
}
