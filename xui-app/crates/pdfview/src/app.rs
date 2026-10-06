//! The PDF Viewer window: its messages, opening documents, and the
//! commands that move and zoom the view.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use lazypdf::{Document, OpenError};
use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::message::{Key, Modifiers};
use xui_core::widget::DialogAction;

use crate::host::Host;
use crate::layout::{self, Zoom};
use crate::ui::{self, Dialogs, Widgets};
use crate::viewer::Viewer;
use crate::worker::Pool;

/// The window size the binary asks for.
pub const WINDOW: (i32, i32) = (820, 640);

/// `=`/`+` and `-`/`_` on the main keyboard (Windows virtual keys, which
/// the LazyOS backend maps the US layout to).
const KEY_PLUS: Key = Key::from_code(0xBB);
const KEY_MINUS: Key = Key::from_code(0xBD);

#[derive(Clone, Debug)]
pub enum Msg {
    Open,
    OpenChosen(PathBuf),
    Password(DialogAction),
    DialogClosed,
    PreviousPage,
    NextPage,
    FirstPage,
    LastPage,
    /// Scroll by a fraction of the viewport: (columns, rows) in tenths.
    ScrollBy(i32, i32),
    GoToPage(usize),
    ZoomStep(bool),
    FitWidth,
    FitPage,
    ActualSize,
    /// The view scrolled itself (wheel, scroll bar).
    Scrolled,
    /// The view's drain timer fired.
    Tick,
    /// Files were dropped on the window.
    Dropped(Vec<PathBuf>),
    Close,
}

pub struct PdfApp {
    host: Host,
    pub(crate) w: Widgets,
    dialogs: Dialogs,
    /// The open document's path.
    path: Option<PathBuf>,
    /// A file waiting for its password.
    locked: Option<(PathBuf, Vec<u8>)>,
    dialog_open: Rc<Cell<bool>>,
}

impl PdfApp {
    pub fn build(ui: &mut Ui<Msg>, host: Host) -> Result<PdfApp> {
        let threads = match host.threads {
            0 => Pool::default_threads(),
            n => n,
        };
        let viewer = Rc::new(RefCell::new(Viewer::new(Rc::clone(&host.log), threads)));
        let (w, dialogs) = ui::build_window(ui, &host, viewer)?;
        ui.on_close(|| Some(Msg::Close));
        let dialog_open = Rc::new(Cell::new(false));
        {
            let dialog_open = Rc::clone(&dialog_open);
            ui.on_key(move |key, modifiers| {
                if dialog_open.get() {
                    None
                } else {
                    shortcut(key, modifiers)
                }
            });
        }
        let app = PdfApp {
            host,
            w,
            dialogs,
            path: None,
            locked: None,
            dialog_open,
        };
        ui.focus(app.w.view.id());
        app.sync();
        Ok(app)
    }

    /// The viewer the page view paints (tests read it).
    pub fn viewer(&self) -> std::cell::Ref<'_, Viewer> {
        self.w.view.viewer.borrow()
    }

    /// A handle on the viewer, for a test to read after the window is built.
    pub fn viewer_handle(&self) -> Rc<RefCell<Viewer>> {
        Rc::clone(&self.w.view.viewer)
    }

    /// Whether render threads still have tiles to hand over.
    pub fn draining(&self) -> bool {
        self.w.view.draining()
    }

    fn log(&self, line: &str) {
        (self.host.log)(line);
    }

    /// Reads and opens `path`; a locked file asks for its password.
    fn open_path(&mut self, path: PathBuf) {
        match std::fs::read(&path) {
            Ok(bytes) => self.open_bytes(path, bytes, ""),
            Err(e) => self.failed(&path, &e.to_string()),
        }
    }

    fn open_bytes(&mut self, path: PathBuf, bytes: Vec<u8>, password: &str) {
        // `Document::open` takes the bytes; a locked file keeps a copy for
        // the next try.
        let retry = bytes.clone();
        match Document::open(bytes, password) {
            Ok(doc) => {
                let pages = doc.page_count();
                self.w.view.viewer.borrow_mut().open(doc);
                self.log(&format!("PDF:OPEN:PASS:{}:{pages}", path.display()));
                self.path = Some(path);
                self.locked = None;
            }
            Err(OpenError::Password) => {
                let name = file_name(&path);
                let text = if password.is_empty() {
                    format!("{name} is protected by a password.")
                } else {
                    format!("That password does not open {name}. Try again:")
                };
                self.log(&format!("PDF:OPEN:PASSWORD:{}", path.display()));
                self.locked = Some((path, retry));
                self.dialogs.password.set_message(&text);
                self.dialogs.password.open();
            }
            Err(e) => self.failed(&path, e.reason()),
        }
    }

    fn failed(&mut self, path: &Path, reason: &str) {
        self.log(&format!("PDF:OPEN:FAIL:{}:{reason}", path.display()));
        let text = format!("{} could not be opened: {reason}.", file_name(path));
        self.w.view.viewer.borrow_mut().fail(text);
        self.path = None;
    }

    /// Mirrors the viewer onto the status bar and asks for tiles.
    fn sync(&self) {
        self.dialog_open.set(self.dialogs.any_open());
        let v = self.viewer();
        let name = self
            .path
            .as_deref()
            .map_or("No document".to_owned(), file_name);
        let (page, zoom) = if v.page_count() == 0 {
            (String::new(), String::new())
        } else {
            (
                format!("Page {} of {}", v.current_page() + 1, v.page_count()),
                format!("{:.0}%", v.factor() * 100.0),
            )
        };
        drop(v);
        self.w.status.set_text(0, &name);
        self.w.status.set_text(1, &page);
        self.w.status.set_text(2, &zoom);
        self.w.view.refresh();
    }

    fn zoom(&self, zoom: Zoom) {
        self.w.view.viewer.borrow_mut().set_zoom(zoom);
        let factor = self.viewer().factor();
        self.log(&format!("PDF:ZOOM:{:.0}", factor * 100.0));
    }

    fn go_to(&self, page: usize) {
        let mut v = self.w.view.viewer.borrow_mut();
        let last = v.page_count().saturating_sub(1);
        v.go_to_page(page.min(last));
    }
}

/// The keyboard: claimed ahead of the focused widget while no dialog is open.
pub fn shortcut(key: Key, m: Modifiers) -> Option<Msg> {
    let plain = !m.ctrl && !m.alt;
    match key {
        Key::O if m.ctrl => Some(Msg::Open),
        KEY_PLUS if m.ctrl => Some(Msg::ZoomStep(true)),
        KEY_MINUS if m.ctrl => Some(Msg::ZoomStep(false)),
        Key::DIGIT0 if m.ctrl => Some(Msg::ActualSize),
        Key::DIGIT1 if m.ctrl => Some(Msg::FitWidth),
        Key::DIGIT2 if m.ctrl => Some(Msg::FitPage),
        Key::HOME if m.ctrl || plain => Some(Msg::FirstPage),
        Key::END if m.ctrl || plain => Some(Msg::LastPage),
        Key::PAGE_UP if m.ctrl => Some(Msg::PreviousPage),
        Key::PAGE_DOWN if m.ctrl => Some(Msg::NextPage),
        Key::P if plain => Some(Msg::PreviousPage),
        Key::N if plain => Some(Msg::NextPage),
        Key::PAGE_UP if plain => Some(Msg::ScrollBy(0, -9)),
        Key::PAGE_DOWN if plain => Some(Msg::ScrollBy(0, 9)),
        Key::SPACE if plain => Some(Msg::ScrollBy(0, if m.shift { -9 } else { 9 })),
        Key::UP if plain => Some(Msg::ScrollBy(0, -1)),
        Key::DOWN if plain => Some(Msg::ScrollBy(0, 1)),
        Key::LEFT if plain => Some(Msg::ScrollBy(-1, 0)),
        Key::RIGHT if plain => Some(Msg::ScrollBy(1, 0)),
        _ => None,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

impl App for PdfApp {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Open => {
                if let Some(dir) = self.path.as_deref().and_then(Path::parent) {
                    self.dialogs.open.set_initial_dir(dir.to_path_buf());
                }
                self.dialogs.open.open();
            }
            Msg::OpenChosen(path) => {
                self.open_path(path);
                ui.focus(self.w.view.id());
            }
            Msg::Dropped(paths) => {
                if let Some(path) = paths.into_iter().next() {
                    self.open_path(path);
                }
            }
            Msg::Password(DialogAction::Accept(password)) => {
                if let Some((path, bytes)) = self.locked.take() {
                    self.open_bytes(path, bytes, &password);
                }
            }
            Msg::Password(DialogAction::Cancel) => {
                if let Some((path, _)) = self.locked.take() {
                    self.failed(&path, "password required");
                }
            }
            Msg::DialogClosed => ui.focus(self.w.view.id()),
            Msg::PreviousPage => {
                let current = self.viewer().current_page();
                self.go_to(current.saturating_sub(1));
            }
            Msg::NextPage => {
                let current = self.viewer().current_page();
                self.go_to(current + 1);
            }
            Msg::FirstPage => self.go_to(0),
            Msg::LastPage => self.go_to(usize::MAX),
            Msg::GoToPage(page) => self.go_to(page),
            Msg::ScrollBy(cols, rows) => {
                let mut v = self.w.view.viewer.borrow_mut();
                // An arrow moves a tenth of the view; a page key nine tenths.
                let (dx, dy) = (v.view.0 * cols / 10, v.view.1 * rows / 10);
                v.scroll_by(dx, dy);
            }
            Msg::ZoomStep(up) => {
                let now = self.viewer().factor();
                self.zoom(Zoom::Factor(layout::step(now, up)));
            }
            Msg::FitWidth => self.zoom(Zoom::FitWidth),
            Msg::FitPage => self.zoom(Zoom::FitPage),
            Msg::ActualSize => self.zoom(Zoom::Factor(1.0)),
            Msg::Scrolled => {}
            Msg::Tick => {
                self.w.view.tick();
                // Tiles do not move the view: nothing else to mirror.
                return;
            }
            Msg::Close => {
                self.log("PDF:QUIT:PASS");
                ui.quit();
                return;
            }
        }
        self.sync();
    }
}
