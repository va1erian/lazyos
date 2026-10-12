#![forbid(unsafe_code)]

//! The application: widget construction and the `Msg` -> model glue.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::message::Key;
use xui_core::widget::{ColorPanel, FileSystem, Menu, StatusBar};

use crate::model::text::TextRasterizer;

use super::canvas::CanvasMsg;
use super::coloredit::ColorEditor;
use super::dialogs::{FlattenPrompt, Frames, UnsavedPrompt};
use super::layerprops::LayerPropsDialog;
use super::layout::{self, Observer};
use super::menu::{self, Enabled};
use super::options::{Options, step_size};
use super::palette::Palette;
use super::shortcuts::{float_key, shortcut, typing_shortcut};
use super::sidebar::Sidebar;
use super::sysclip::SystemClipboard;
use super::toolbar::ToolStrip;
use super::{Msg, PaintCanvas};
use crate::model::{Format, LayerProps, Model, Pixel, Side, Tool};
use crate::storage::Storage;

use crate::{DEFAULT_HEIGHT, DEFAULT_WIDTH};

/// The size of the text block drag handle on screen, in pixels.
const HANDLE_SCREEN_PX: f64 = 10.0;

mod adjust;
mod canvas_msgs;
mod colors;
mod commands;
mod dialogs;
mod doc_io;
mod files;
mod guard;
mod image_dialogs;
mod layers;
mod probe;
mod report;
mod save_flow;
mod setup;
mod status;

use canvas_msgs::{ZoomDrag, keeps_text, returns_focus};
use commands::PasteKind;
use dialogs::{AboutBox, FileDialogs, PasteSizePrompt};
use guard::After;
pub use report::Report;
use report::Reporter;
pub use setup::Setup;

/// The paint application.
pub struct PaintApp {
    model: Model,
    menu: Rc<Menu<Msg>>,
    options: Options,
    canvas: Rc<PaintCanvas>,
    toolbar: Rc<ToolStrip>,
    palette: Rc<Palette>,
    editor: ColorEditor,
    props: LayerPropsDialog,
    sidebar: Sidebar,
    /// The layer whose properties are being edited and what it had.
    editing_layer: Option<(usize, LayerProps)>,
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
    frames: Frames,
    unsaved: UnsavedPrompt,
    flatten_prompt: FlattenPrompt,
    /// The revision the file (or a fresh start) has; `None` when the picture
    /// has no counterpart on disk to be in step with.
    saved_revision: Option<u64>,
    /// What the unsaved-changes guard is holding back.
    after: Option<After>,
    /// The write the "Flatten image?" card is waiting to confirm.
    pending_save: Option<(PathBuf, Format)>,
    /// The host's evidence hook.
    reporter: Option<Reporter>,
    about: AboutBox,
    /// Where [`Msg::Probe`] reports the named controls, if a host asked.
    probe: Option<probe::Hook>,
    /// The file the document was last opened from or saved to.
    current_path: Option<PathBuf>,
    /// The last window title set, so an unchanged one is not re-sent.
    title: String,
    /// Whether the arrow keys, Enter and Esc belong to a move tool or a float.
    float_keys: Rc<Cell<bool>>,
    /// Whether a text block is being edited, so letters are text, not tools.
    typing: Rc<Cell<bool>>,
    /// The Zoom tool drag, while the button is down.
    zoom_drag: Option<ZoomDrag>,
    /// The OK and Cancel messages of the open Image or Adjustments frame.
    frame_keys: Rc<Cell<Option<(Msg, Msg)>>>,
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
        PaintApp::construct(ui, storage, observer, files, None)
    }

    /// Like [`PaintApp::build`], additionally reporting state to `observer`.
    pub fn build_observed(
        ui: &mut Ui<Msg>,
        storage: Rc<dyn Storage>,
        observer: Rc<RefCell<Observer>>,
    ) -> Result<PaintApp> {
        PaintApp::construct(ui, storage, observer, None, None)
    }

    /// Builds the app from every host choice at once (see [`Setup`]).
    pub fn build_with(ui: &mut Ui<Msg>, setup: Setup) -> Result<PaintApp> {
        let files = setup.storage.supports_paths().then_some(setup.fs).flatten();
        PaintApp::construct(ui, setup.storage, setup.observer, files, setup.text)
    }

    fn construct(
        ui: &mut Ui<Msg>,
        storage: Rc<dyn Storage>,
        observer: Rc<RefCell<Observer>>,
        fs: Option<Rc<dyn FileSystem>>,
        text: Option<Rc<dyn TextRasterizer>>,
    ) -> Result<PaintApp> {
        let files = fs.map(|fs| FileDialogs::new(ui, fs)).transpose()?;
        let families = text.as_ref().map(|t| t.families()).unwrap_or_default();
        let layout::Widgets {
            menu,
            options,
            canvas,
            toolbar,
            palette,
            editor,
            props,
            frames,
            sidebar,
            status,
        } = layout::mount(ui, families)?;
        let modal = Rc::new(Cell::new(false));
        let float_keys = Rc::new(Cell::new(false));
        let typing = Rc::new(Cell::new(false));
        let frame_keys: Rc<Cell<Option<(Msg, Msg)>>> = Rc::new(Cell::new(None));
        {
            let modal = Rc::clone(&modal);
            let float_keys = Rc::clone(&float_keys);
            let typing = Rc::clone(&typing);
            let frame_keys = Rc::clone(&frame_keys);
            // Weak: the menu holds the window, and the window holds this hook.
            let menu = Rc::downgrade(&menu);
            ui.on_key(move |key, modifiers| {
                let menu_open = menu.upgrade().is_some_and(|menu| menu.is_open());
                let blocked = modal.get() || menu_open;
                // An Image or Adjustments dialog is a frame, not a card, so
                // Enter and Esc are routed to it here, and nothing else (no
                // tool letter) gets past it: `modal` blocks the table.
                if let Some((ok, cancel)) = frame_keys.get().filter(|_| !menu_open) {
                    match key {
                        Key::RETURN => return Some(ok),
                        Key::ESCAPE => return Some(cancel),
                        _ => {}
                    }
                }
                let chosen = if typing.get() {
                    typing_shortcut(key, modifiers, blocked)
                } else {
                    shortcut(key, modifiers, blocked)
                };
                chosen.or_else(|| {
                    (float_keys.get() && !blocked)
                        .then(|| float_key(key, modifiers))
                        .flatten()
                })
            });
        }

        let mut model = Model::new(DEFAULT_WIDTH, DEFAULT_HEIGHT);
        if let Some(text) = text {
            model.set_text_rasterizer(text);
        }
        let mut app = PaintApp {
            model,
            menu,
            options,
            canvas,
            toolbar,
            palette,
            editor,
            props,
            sidebar,
            editing_layer: None,
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
            frames,
            unsaved: UnsavedPrompt::default(),
            flatten_prompt: FlattenPrompt::default(),
            saved_revision: None,
            after: None,
            pending_save: None,
            reporter: None,
            about: AboutBox::default(),
            probe: None,
            current_path: None,
            title: String::new(),
            float_keys,
            typing,
            zoom_drag: None,
            frame_keys,
            paste_size: PasteSizePrompt::default(),
            pending_paste: None,
            system: None,
            last_offered: None,
            local_newest: false,
        };
        app.mark_clean();
        // The window's close button asks like File > Quit does.
        ui.on_close(|| Some(Msg::Quit));
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
            layers: self.model.document().layers().len(),
            active: self.model.document().active(),
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
        // The text handle keeps its size on screen at every zoom.
        let zoom = self.canvas.viewport().zoom();
        self.model
            .set_text_handle_px((HANDLE_SCREEN_PX / zoom).ceil() as u32);
        self.canvas.sync(&self.model);
        self.toolbar.sync(&self.model);
        self.palette
            .sync(self.model.primary(), self.model.secondary());
        self.options.sync(ui, &self.model);
        self.sidebar.sync(ui, &self.model);
        menu::sync(&self.menu, self.enabled());
        self.modal.set(self.modal_open(ui));
        self.frame_keys.set(self.frames.keys(ui));
        self.float_keys
            .set(self.model.tool().is_move() || self.model.is_floating());
        self.typing.set(self.model.is_editing_text());
        if let Some(notice) = self.model.take_notice() {
            self.message = Some(notice);
        }
        self.refresh_status();
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
                | Msg::Resize(_)
                | Msg::CanvasSize(_)
                | Msg::Adjust(..)
                | Msg::Unsaved(_)
                | Msg::Flatten(_)
                | Msg::DialogClosed
                | Msg::OpenStartup
                | Msg::ColorEditing(_)
                | Msg::ColorDone
                | Msg::ColorCancel
                | Msg::ColorAlpha(_)
                | Msg::PropsChanged
                | Msg::PropsOk
                | Msg::PropsCancel
                | Msg::Probe
                | Msg::PasteExpand
                | Msg::PasteKeep
                | Msg::Canvas(CanvasMsg::Cancel)
        );
        if !passes && self.modal_open(ui) {
            return;
        }
        // Anything but canvas input ends a stroke in progress, and keeps a
        // text block as pixels unless the message is about the block.
        if !matches!(msg, Msg::Canvas(_)) && self.model.is_dragging() {
            self.model.end();
        }
        if self.model.is_editing_text() && !keeps_text(&msg) {
            self.model.commit_text();
        }
        match msg {
            Msg::Tool(tool) => self.select_tool(tool),
            Msg::CycleTool(group) => self.select_tool(group.next(self.model.tool())),
            Msg::Size(size) => self.model.set_size(size),
            Msg::Opt(opt) => opt.apply(&mut self.model, self.options.families()),
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
            Msg::ColorAlpha(alpha) => self.color_alpha(alpha),
            Msg::PropsChanged => self.props_changed(),
            Msg::PropsOk => self.close_props(ui, true),
            Msg::PropsCancel => self.close_props(ui, false),
            Msg::LayerSelect(_) | Msg::LayerEye(_) | Msg::LayerProps(_) => {
                self.layer_msg(ui, msg);
            }
            Msg::HistoryToggle => self.sidebar.toggle(ui),
            Msg::HistoryJump(applied) => self.history_jump(applied),
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
            Msg::Clear => self.message = self.model.clear().err(),
            Msg::New => self.guard(ui, After::New),
            Msg::NewTransparent => self.guard(ui, After::NewTransparent),
            Msg::Unsaved(action) => self.unsaved_answer(ui, action),
            Msg::Flatten(action) => self.flatten_answer(ui, action),
            Msg::Save => self.save_requested(ui),
            Msg::SaveAs => self.save_as_requested(),
            Msg::Open => self.guard(ui, After::Open),
            Msg::Quit => self.guard(ui, After::Quit),
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
            Msg::CanvasSizeAsk => self.open_canvas_size(ui),
            Msg::Resize(msg) => self.resize_msg(ui, msg),
            Msg::CanvasSize(msg) => self.canvas_size_msg(ui, msg),
            Msg::Adjust(kind, msg) => self.adjust_msg(ui, kind, msg),
            Msg::DialogClosed => {
                // A cancelled picker or card also cancels what it was for.
                self.after = None;
                ui.focus(self.canvas.id());
            }
            Msg::Nudge(..)
            | Msg::FloatCommit
            | Msg::FloatCancel
            | Msg::SelectMode(_)
            | Msg::WandTolerance(_)
            | Msg::WandContiguous(_)
            | Msg::PasteExpand
            | Msg::PasteKeep => self.selection_msg(msg),
            Msg::Canvas(canvas) => self.canvas_msg(canvas),
        }
        // A control that took the keyboard gives it back, so the next key is
        // still text.
        if self.model.is_editing_text() && returns_focus(&msg) {
            ui.focus(self.canvas.id());
        }
        self.sync(ui);
        if self.title != self.title() {
            self.title = self.title();
            ui.set_window_title(&self.title);
            self.probe(ui);
        }
    }
}
