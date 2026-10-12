#![forbid(unsafe_code)]

//! The application: widget construction and the `Msg` -> model glue.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::widget::{ColorPanel, FileSystem, Menu, StatusBar};

use super::canvas::CanvasMsg;
use super::coloredit::ColorEditor;
use super::layout::{self, Observer};
use super::menu::{self, Enabled};
use super::options::{Options, step_size};
use super::palette::Palette;
use super::shortcuts::{float_key, shortcut, tool_hint};
use super::sysclip::SystemClipboard;
use super::toolbar::ToolStrip;
use super::{Msg, PaintCanvas};
use crate::model::{Model, Pixel, Side, Tool};
use crate::storage::Storage;

use crate::{DEFAULT_HEIGHT, DEFAULT_WIDTH};

mod colors;
mod commands;
mod dialogs;
mod files;
mod probe;

use commands::PasteKind;
use dialogs::{AboutBox, FileDialogs, PasteSizePrompt, ResizePrompt};

/// The paint application.
pub struct PaintApp {
    model: Model,
    menu: Rc<Menu<Msg>>,
    options: Options,
    canvas: Rc<PaintCanvas>,
    toolbar: Rc<ToolStrip>,
    palette: Rc<Palette>,
    editor: ColorEditor,
    status: Rc<StatusBar<Msg>>,
    storage: Rc<dyn Storage>,
    observer: Rc<RefCell<Observer>>,
    cursor: Option<(i32, i32)>,
    message: Option<String>,
    /// Whether the pixel grid is on (View > Pixel Grid).
    grid: bool,
    /// The tool under the pointer in the tool grid, named in the status bar.
    hint: Option<Tool>,
    /// The colour being edited and the value it had when the edit began.
    editing: Option<(Side, Pixel)>,
    /// Whether a dialog, the editor or a menu has the keyboard; the window's key
    /// hook reads it so shortcuts stay out of their way.
    modal: Rc<Cell<bool>>,
    /// The Open/Save As pickers; `None` keeps the path-less Save/Open.
    files: Option<FileDialogs>,
    resize: ResizePrompt,
    about: AboutBox,
    /// Where [`Msg::Probe`] reports the named controls, if a host asked.
    probe: Option<probe::Hook>,
    /// The file the document was last opened from or saved to.
    current_path: Option<PathBuf>,
    /// The last window title set, so an unchanged one is not re-sent.
    title: String,
    /// Whether the arrow keys, Enter and Esc belong to a move tool or a float.
    float_keys: Rc<Cell<bool>>,
    paste_size: PasteSizePrompt,
    /// The paste waiting for the oversized-picture answer.
    pending_paste: Option<PasteKind>,
    /// The system `image/png` clipboard, if the host wired one.
    system: Option<Rc<dyn SystemClipboard>>,
    /// The PNG last offered to it; what is there now is ours when it matches.
    last_offered: Option<Vec<u8>>,
    /// Whether the newest copy is only in the in-app slot (too big to offer).
    local_newest: bool,
}

impl PaintApp {
    /// Builds the widgets for `ui` and wires them to a fresh model.
    pub fn build(ui: &mut Ui<Msg>, storage: Rc<dyn Storage>) -> Result<PaintApp> {
        PaintApp::build_observed(ui, storage, Rc::new(RefCell::new(Observer::default())))
    }

    /// Like [`PaintApp::build`], with Open and Save As file dialogs over `fs`.
    /// The storage must support paths ([`Storage::supports_paths`]) for the
    /// dialogs to be wired; otherwise this is [`PaintApp::build`].
    pub fn build_with_files(
        ui: &mut Ui<Msg>,
        storage: Rc<dyn Storage>,
        fs: Rc<dyn FileSystem>,
    ) -> Result<PaintApp> {
        let observer = Rc::new(RefCell::new(Observer::default()));
        PaintApp::build_with_files_observed(ui, storage, fs, observer)
    }

    /// Like [`PaintApp::build_with_files`], additionally reporting to `observer`.
    pub fn build_with_files_observed(
        ui: &mut Ui<Msg>,
        storage: Rc<dyn Storage>,
        fs: Rc<dyn FileSystem>,
        observer: Rc<RefCell<Observer>>,
    ) -> Result<PaintApp> {
        let files = storage.supports_paths().then_some(fs);
        PaintApp::construct(ui, storage, observer, files)
    }

    /// Sets the directory the Open and Save As dialogs start in.
    pub fn set_start_dir(&self, dir: impl Into<PathBuf>) {
        if let Some(files) = &self.files {
            files.set_start_dir(&dir.into());
        }
    }

    /// Like [`PaintApp::build`], additionally reporting state to `observer`.
    pub fn build_observed(
        ui: &mut Ui<Msg>,
        storage: Rc<dyn Storage>,
        observer: Rc<RefCell<Observer>>,
    ) -> Result<PaintApp> {
        PaintApp::construct(ui, storage, observer, None)
    }

    fn construct(
        ui: &mut Ui<Msg>,
        storage: Rc<dyn Storage>,
        observer: Rc<RefCell<Observer>>,
        fs: Option<Rc<dyn FileSystem>>,
    ) -> Result<PaintApp> {
        let files = fs.map(|fs| FileDialogs::new(ui, fs)).transpose()?;
        let layout::Widgets {
            menu,
            options,
            canvas,
            toolbar,
            palette,
            editor,
            status,
        } = layout::mount(ui)?;
        let modal = Rc::new(Cell::new(false));
        let float_keys = Rc::new(Cell::new(false));
        {
            let modal = Rc::clone(&modal);
            let float_keys = Rc::clone(&float_keys);
            // Weak: the menu holds the window, and the window holds this hook.
            let menu = Rc::downgrade(&menu);
            ui.on_key(move |key, modifiers| {
                let menu_open = menu.upgrade().is_some_and(|menu| menu.is_open());
                let blocked = modal.get() || menu_open;
                shortcut(key, modifiers, blocked).or_else(|| {
                    (float_keys.get() && !blocked)
                        .then(|| float_key(key, modifiers))
                        .flatten()
                })
            });
        }

        let mut app = PaintApp {
            model: Model::new(DEFAULT_WIDTH, DEFAULT_HEIGHT),
            menu,
            options,
            canvas,
            toolbar,
            palette,
            editor,
            status,
            storage,
            observer,
            cursor: None,
            message: None,
            grid: false,
            hint: None,
            editing: None,
            modal,
            files,
            resize: ResizePrompt::default(),
            about: AboutBox::default(),
            probe: None,
            current_path: None,
            title: String::new(),
            float_keys,
            paste_size: PasteSizePrompt::default(),
            pending_paste: None,
            system: None,
            last_offered: None,
            local_newest: false,
        };
        // Rows of unbuilt features start disabled; the live ones follow the model.
        menu::init(&app.menu, app.enabled());
        // Shortcuts reach the app through the focused widget's key events on some
        // backends, so the canvas has the keyboard from the start.
        ui.focus(app.canvas.id());
        app.sync(ui);
        Ok(app)
    }

    /// Whether Open and Save have something behind them.
    fn io(&self) -> bool {
        self.storage.available() || self.files.is_some()
    }

    /// What the menu's live rows depend on.
    fn enabled(&self) -> Enabled {
        Enabled {
            can_undo: self.model.history().can_undo(),
            can_redo: self.model.history().can_redo(),
            io: self.io(),
            selection: self.model.selection().is_some(),
            can_paste: self.model.clipboard().is_some() || self.system.is_some(),
        }
    }

    /// The drawing model.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The canvas widget.
    pub fn canvas(&self) -> &PaintCanvas {
        &self.canvas
    }

    /// The tool grid.
    pub fn toolbar(&self) -> &ToolStrip {
        &self.toolbar
    }

    /// The colour editor's panel (it is hidden until a swatch is clicked).
    pub fn color_panel(&self) -> Rc<ColorPanel<Msg>> {
        self.editor.panel()
    }

    /// The menu bar.
    pub fn menu(&self) -> Rc<Menu<Msg>> {
        Rc::clone(&self.menu)
    }

    /// The palette.
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// The status bar.
    pub fn status(&self) -> &StatusBar<Msg> {
        &self.status
    }

    /// The last save/load message, if any.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Rebuilds the painter state, tool grid, menu, options bar and status bar
    /// from the model.
    fn sync(&mut self, ui: &Ui<Msg>) {
        self.canvas.sync(&self.model);
        self.toolbar.sync(&self.model);
        self.palette
            .sync(self.model.primary(), self.model.secondary());
        self.options.sync(ui, &self.model);
        menu::sync(&self.menu, self.enabled());
        self.modal.set(self.modal_open(ui));
        self.float_keys
            .set(self.model.tool().is_move() || self.model.is_floating());
        if let Some(notice) = self.model.take_notice() {
            self.message = Some(notice);
        }
        self.refresh_status();
    }

    /// Rewrites the status bar parts.
    fn refresh_status(&mut self) {
        let position = match self.cursor {
            Some((x, y)) => format!("{x}, {y}"),
            None => "--".to_string(),
        };
        let (width, height) = self.model.document().size();
        let selection = self.model.selection().map(|s| s.bounds());
        let size = match selection {
            Some(b) => format!("{width} x {height}  sel {}x{}", b.width(), b.height()),
            None => format!("{width} x {height}"),
        };
        let tool = self.model.tool().label().to_string();
        // A hovered tool names itself over the last message while the pointer is
        // on the grid.
        let message = match self.hint {
            Some(tool) => tool_hint(tool),
            None => self.message.clone().unwrap_or_default(),
        };
        self.status.set_text(0, &position);
        self.status.set_text(1, &size);
        self.status.set_text(2, &tool);
        self.status.set_text(3, &message);
        let zoom_percent = self.canvas.zoom_percent();
        let zoom = format!("{zoom_percent}%");
        self.status.set_text(4, &zoom);

        let mut observer = self.observer.borrow_mut();
        observer.tool = self.model.tool();
        observer.size = self.model.size();
        observer.primary = self.model.primary();
        observer.secondary = self.model.secondary();
        observer.can_undo = self.model.history().can_undo();
        observer.can_redo = self.model.history().can_redo();
        observer.cursor = self.cursor;
        observer.dragging = self.model.is_dragging();
        observer.selection = selection.map(|b| (b.x0, b.y0, b.width(), b.height()));
        observer.floating = self.model.is_floating();
        observer.zoom_percent = zoom_percent;
        observer.editing = self.editing.map(|(side, _)| side);
        observer.status = [position, size, tool, message, zoom];
    }

    /// Saves the bitmap through the storage, reporting the outcome.
    fn save(&mut self) {
        let result = self
            .model
            .flatten()
            .encode_png()
            .map_err(|error| error.to_string())
            .and_then(|bytes| self.storage.save(&bytes));
        self.message = Some(match result {
            Ok(()) => "Saved".to_string(),
            Err(error) => format!("Save failed: {error}"),
        });
    }

    /// Decodes first, then swaps, so a failed load leaves the canvas intact.
    fn open(&mut self) -> bool {
        let Some(bytes) = self.storage.load() else {
            self.message = Some("Open failed: nothing stored".to_string());
            return false;
        };
        match crate::model::Bitmap::decode(&bytes) {
            Ok(bitmap) => match self.model.load(bitmap) {
                Ok(()) => {
                    self.cursor = None;
                    self.message = Some("Opened".to_string());
                    true
                }
                Err(error) => {
                    self.message = Some(format!("Open failed: {error}"));
                    false
                }
            },
            Err(error) => {
                self.message = Some(format!("Open failed: {error}"));
                false
            }
        }
    }

    /// Applies the size typed into the Resize prompt.
    fn resize_chosen(&mut self, ui: &Ui<Msg>) {
        ui.focus(self.canvas.id());
        let Some(text) = self.resize.take_text() else {
            return;
        };
        let result = crate::model::parse_size(&text)
            .and_then(|(width, height)| self.model.resize(width, height).map(|()| (width, height)));
        self.message = Some(match result {
            Ok((width, height)) => format!("Resized to {width} x {height}"),
            Err(error) => format!("Resize failed: {error}"),
        });
    }
}

impl App for PaintApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        // A dialog (or the colour editor) is modal: only its own results and a
        // focus-loss cancel pass.
        let passes = matches!(
            msg,
            Msg::OpenChosen
                | Msg::SaveChosen
                | Msg::ResizeChosen
                | Msg::DialogClosed
                | Msg::OpenStartup
                | Msg::ColorEditing(_)
                | Msg::ColorDone
                | Msg::ColorCancel
                | Msg::Probe
                | Msg::PasteExpand
                | Msg::PasteKeep
                | Msg::Canvas(CanvasMsg::Cancel)
        );
        if !passes && self.modal_open(ui) {
            return;
        }
        // Anything but canvas input ends a stroke in progress.
        if !matches!(msg, Msg::Canvas(_)) && self.model.is_dragging() {
            self.model.end();
        }
        match msg {
            Msg::Tool(tool) => self.select_tool(tool),
            Msg::CycleTool(group) => self.select_tool(group.next(self.model.tool())),
            Msg::Size(size) => self.model.set_size(size),
            Msg::SizeStep(steps) => self.model.set_size(step_size(self.model.size(), steps)),
            Msg::Palette { color, side } => match side {
                Side::Primary => self.model.set_primary(color),
                Side::Secondary => self.model.set_secondary(color),
            },
            Msg::SwapColors => self.model.swap_colors(),
            Msg::ResetColors => self.reset_colors(),
            Msg::HintTool(tool) => self.hint = tool,
            Msg::EditColor(side) => self.edit_color(ui, side),
            Msg::ColorEditing(color) => self.color_editing(color),
            Msg::ColorDone => self.close_editor(ui, true),
            Msg::ColorCancel => self.close_editor(ui, false),
            Msg::Undo => {
                // The menu cannot relabel a row ("Undo Paintbrush"), so the
                // status bar names what was undone.
                let name = self.model.history().undo_name().map(str::to_owned);
                self.model.undo();
                self.message = name.map(|name| format!("Undid {name}"));
            }
            Msg::Redo => {
                let name = self.model.history().redo_name().map(str::to_owned);
                self.model.redo();
                self.message = name.map(|name| format!("Redid {name}"));
            }
            Msg::Clear => self.model.clear(),
            Msg::New => match self.model.reset(DEFAULT_WIDTH, DEFAULT_HEIGHT) {
                Ok(()) => {
                    self.cursor = None;
                    self.current_path = None;
                }
                Err(error) => self.message = Some(format!("New failed: {error}")),
            },
            Msg::Save => self.save_requested(),
            Msg::SaveAs => self.save_as_requested(),
            Msg::Open => self.open_requested(),
            Msg::Quit => ui.quit(),
            Msg::About => {
                if let Err(error) = self.about.show(ui) {
                    self.message = Some(format!("About failed: {error}"));
                }
            }
            Msg::Command(command) => self.run_command(ui, command),
            Msg::Probe => self.probe(ui),
            Msg::OpenStartup => self.open_startup(),
            Msg::OpenChosen => self.open_chosen(ui),
            Msg::SaveChosen => self.save_chosen(ui),
            Msg::ResizeAsk => {
                if let Err(error) = self.resize.show(ui, self.model.document().size()) {
                    self.message = Some(format!("Resize failed: {error}"));
                }
            }
            Msg::ResizeChosen => self.resize_chosen(ui),
            Msg::DialogClosed => ui.focus(self.canvas.id()),
            Msg::Nudge(..)
            | Msg::FloatCommit
            | Msg::FloatCancel
            | Msg::SelectMode(_)
            | Msg::WandTolerance(_)
            | Msg::WandContiguous(_)
            | Msg::PasteExpand
            | Msg::PasteKeep => self.selection_msg(msg),
            Msg::Canvas(CanvasMsg::Down { x, y, side, mods }) => {
                if self.model.is_dragging() {
                    self.model.end();
                }
                self.hint = None;
                self.cursor = Some((x, y));
                self.model.begin_with(x, y, side, mods);
            }
            Msg::Canvas(CanvasMsg::Move { x, y, mods }) => {
                // The pointer is on the canvas, so it is no longer on the grid
                // (a backend need not send the grid a leave).
                self.hint = None;
                self.cursor = Some((x, y));
                self.model.extend_with(x, y, mods);
            }
            Msg::Canvas(CanvasMsg::Up { x, y, mods }) => {
                self.cursor = Some((x, y));
                self.model.extend_with(x, y, mods);
                self.model.end();
            }
            Msg::Canvas(CanvasMsg::Cancel) => {
                self.model.cancel();
                self.cursor = None;
            }
            // The canvas zoomed or scrolled itself; the sync below refreshes
            // the zoom part of the status bar.
            Msg::Canvas(CanvasMsg::View) => {}
        }
        self.sync(ui);
        if self.files.is_some() && self.title != self.title() {
            self.title = self.title();
            ui.set_window_title(&self.title);
            self.probe(ui);
        }
    }
}
