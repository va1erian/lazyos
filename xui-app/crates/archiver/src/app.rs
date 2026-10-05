//! The Archiver window: its messages, its state, and the mirror of that state
//! onto the widgets. The commands themselves are in [`crate::commands`].

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use lazyarc::{Archive, Level};
use xui_core::app::{App, Ui};
use xui_core::backend::{Result, TimerId};
use xui_core::geometry::Point;
use xui_core::message::{Key, Modifiers};
use xui_core::widget::{DialogAction, MenuId, SortDirection, TaskDialogAction};
use xui_core::HasText;

use crate::cells::Cells;
use crate::drag::DragState;
use crate::folder::{self, Column, Row};
use crate::host::Host;
use crate::job::Job;
use crate::ui::{self, Dialogs, Widgets};

/// The list view's header height (xui's `ListView` draws it at 24 dip).
const LIST_HEADER: xui_core::Dip = xui_core::Dip(24.0);

/// The window size the binary asks for.
pub const WINDOW: (i32, i32) = (860, 540);

/// Everything the window reacts to.
#[derive(Clone, Debug)]
pub enum Msg {
    // The toolbar and shortcuts.
    Open,
    New,
    Add,
    Extract,
    Test,
    Delete,
    Up,
    Level(usize),
    // The list.
    Selection(Vec<usize>),
    Activate(usize),
    Sort(usize),
    Context(usize, Point),
    Menu(MenuId),
    // The dialogs.
    OpenChosen(PathBuf),
    NewChosen(PathBuf),
    AddChosen(PathBuf),
    ExtractTo(DialogAction),
    DeleteConfirmed(TaskDialogAction),
    PickerClosed,
    MessageClosed,
    // The running job.
    Tick,
    Cancel,
    // Drag and drop, from the platform.
    /// A drag carrying files entered the window.
    DragEnter,
    /// The drag left the window (or ended elsewhere).
    DragLeave,
    /// Files were dropped on the window.
    Dropped(Vec<PathBuf>),
    /// A drag out of the list started carrying these rows: show them
    /// selected again (the press collapsed the selection).
    DragStarted(Vec<usize>),
    /// A drag out of the list could not be prepared.
    DragFailed(String),
    /// The window was asked to close.
    Close,
}

/// The Archiver.
pub struct ArchiverApp {
    pub(crate) host: Host,
    pub(crate) w: Widgets,
    pub(crate) dialogs: Dialogs,
    pub(crate) archive: Option<Arc<Archive>>,
    /// The archive folder shown (`""` is the root).
    pub(crate) folder: String,
    pub(crate) rows: Vec<Row>,
    pub(crate) selection: Vec<usize>,
    pub(crate) previous: Vec<usize>,
    pub(crate) sort: (Column, bool),
    pub(crate) level: Level,
    pub(crate) job: Option<Job>,
    pub(crate) timer: Option<TimerId>,
    /// Files waiting for the New picker to name their archive.
    pub(crate) pending: Vec<PathBuf>,
    /// The row a context menu was opened on.
    pub(crate) context_row: Option<usize>,
    /// A drag with files is over the window.
    pub(crate) hover: bool,
    pub(crate) status: String,
    pub(crate) drag: Rc<RefCell<DragState>>,
    /// Set while a dialog is open, so the window's shortcuts stay off.
    dialog_open: Rc<Cell<bool>>,
    /// The rows last pushed to the list, so an unrelated update keeps its
    /// scroll position.
    rendered: Option<(Vec<Row>, String)>,
}

impl ArchiverApp {
    /// Build the window over `host`; `drag` is the bridge the platform's
    /// drag-and-drop hooks read.
    pub fn build(
        ui: &mut Ui<Msg>,
        host: Host,
        drag: Rc<RefCell<DragState>>,
    ) -> Result<ArchiverApp> {
        let (w, dialogs) = ui::build(ui, &host)?;
        ui.on_close(|| Some(Msg::Close));
        ui.on_timer(|_| Some(Msg::Tick));
        let dialog_open = Rc::new(Cell::new(false));
        {
            let dialog_open = Rc::clone(&dialog_open);
            ui.on_key(move |key, modifiers| {
                if dialog_open.get() {
                    None
                } else {
                    ArchiverApp::shortcut(key, modifiers)
                }
            });
        }
        {
            let mut bridge = drag.borrow_mut();
            bridge.list = Some(w.list.id());
            bridge.list_header = LIST_HEADER.to_px(ui.dpi()).value();
        }
        let mut app = ArchiverApp {
            host,
            w,
            dialogs,
            archive: None,
            folder: String::new(),
            rows: Vec::new(),
            selection: Vec::new(),
            previous: Vec::new(),
            sort: (Column::Name, true),
            level: Level::Normal,
            job: None,
            timer: None,
            pending: Vec::new(),
            context_row: None,
            hover: false,
            status: "Ready".to_owned(),
            drag,
            dialog_open,
            rendered: None,
        };
        app.sync(ui);
        Ok(app)
    }

    /// The key bindings, claimed ahead of the focused widget while no dialog
    /// is open.
    pub fn shortcut(key: Key, modifiers: Modifiers) -> Option<Msg> {
        match key {
            Key::BACK if !modifiers.ctrl && !modifiers.alt => Some(Msg::Up),
            Key::DELETE => Some(Msg::Delete),
            Key::O if modifiers.ctrl => Some(Msg::Open),
            Key::N if modifiers.ctrl => Some(Msg::New),
            Key::E if modifiers.ctrl => Some(Msg::Extract),
            _ => None,
        }
    }

    /// The rows of the current folder, sorted, with `..` below the root.
    pub(crate) fn refresh_rows(&mut self) {
        let mut rows = match &self.archive {
            Some(archive) => folder::rows(&archive.entries, &self.folder),
            None => Vec::new(),
        };
        if self.archive.is_some() && !self.folder.is_empty() {
            rows.insert(0, folder::parent_row());
        }
        folder::sort(&mut rows, self.sort.0, self.sort.1);
        self.rows = rows;
        self.selection.clear();
        self.previous.clear();
    }

    /// Show `text` in the status bar's first part.
    pub(crate) fn say(&mut self, text: impl Into<String>) {
        self.status = text.into();
    }

    /// Show a message box.
    pub(crate) fn tell(&mut self, title: &str, text: &str) {
        self.dialogs.message.set_title(title);
        self.dialogs.message.set_message(text);
        self.dialogs.message.open();
    }

    /// Mirror the state onto the widgets and the drag bridge.
    pub(crate) fn sync(&mut self, ui: &mut Ui<Msg>) {
        let shown = (self.rows.clone(), self.folder.clone());
        if self.rendered.as_ref() != Some(&shown) {
            self.w.list.set_model(Cells::new(&self.rows));
            self.rendered = Some(shown);
        }
        self.w.list.set_selection(&self.selection);
        let open = self.archive.is_some();
        ui.set_visible(self.w.list.id(), open);
        ui.set_visible(self.w.welcome.id(), !open);
        let title = match &self.archive {
            Some(archive) => {
                let name = archive
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let address = if self.folder.is_empty() {
                    name.clone()
                } else {
                    format!("{name} / {}", self.folder.replace('/', " / "))
                };
                self.w.address.set_text(&address);
                format!("{name} - Archiver")
            }
            None => {
                self.w.address.set_text("No archive open");
                "Archiver".to_owned()
            }
        };
        ui.set_window_title(&title);
        let running = self.job.is_some();
        for id in [
            self.w.progress.id(),
            self.w.progress_label.id(),
            self.w.cancel.id(),
        ] {
            ui.set_visible(id, running);
        }
        if let Some(job) = &self.job {
            let snap = job.progress.snapshot();
            self.w.progress.set_value(snap.scaled(1000));
            let name = snap.current.rsplit('/').next().unwrap_or("").to_owned();
            self.w
                .progress_label
                .set_text(&format!("{} {name}", job.task.verb()));
        }
        let status = if self.hover {
            match &self.archive {
                Some(archive) if archive.format.writable() && !archive.format.single_file() => {
                    "Drop to add to this archive".to_owned()
                }
                Some(_) => "Drop an archive to open it (this one is read-only)".to_owned(),
                None => "Drop to open an archive or make a new one".to_owned(),
            }
        } else {
            self.status.clone()
        };
        self.w.status.set_text(0, &status);
        let summary = if open {
            folder::summary(&self.rows, &self.selection)
        } else {
            String::new()
        };
        self.w.status.set_text(1, &summary);
        let format = self.archive.as_ref().map(|a| {
            let access = if a.format.writable() {
                ""
            } else {
                ", read-only"
            };
            format!("{}{access}", a.format.name())
        });
        self.w.status.set_text(2, format.as_deref().unwrap_or(""));
        self.dialog_open.set(self.dialogs.any_open());
        let mut drag = self.drag.borrow_mut();
        drag.archive = self.archive.clone();
        drag.folder = self.folder.clone();
        drag.rows = self.rows.clone();
        drag.selection = self.selection.clone();
        drag.previous = self.previous.clone();
        drag.busy = self.job.is_some();
    }

    /// The sort the list's header shows, as `(column, ascending)`.
    pub(crate) fn header_sort(&self, fallback: usize) -> (Column, bool) {
        match self.w.list.sort_indicator() {
            Some((column, direction)) => (
                Column::from_index(column),
                direction == SortDirection::Ascending,
            ),
            None => (Column::from_index(fallback), true),
        }
    }
}

impl App for ArchiverApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        if !matches!(msg, Msg::Tick | Msg::Selection(_)) {
            (self.host.log)(&format!("ARCHIVER:MSG:{}", short(&msg)));
        }
        crate::commands::dispatch(self, msg, ui);
        self.sync(ui);
    }
}

/// A message's name for the evidence log (no paths or contents).
fn short(msg: &Msg) -> String {
    let text = format!("{msg:?}");
    text.split(['(', ' ', '{']).next().unwrap_or("").to_owned()
}
