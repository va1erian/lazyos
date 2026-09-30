//! The Docs window: a slim toolbar (an Open button and the current path) above
//! the litehtml page, and the Open dialog behind the button and `Ctrl+O`.
//!
//! Serial evidence (the screenshot sessions wait on it): `DOCS:RENDER:PASS` once
//! litehtml's first frame has been painted, `DOCS:OPEN:PASS:<path>` after a
//! document loads, `DOCS:OPEN:FAIL:<path>` when it cannot be read, and
//! `DOCS:LINK:<href>` on a link click.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use xui_app::platform::dialog_fs::LazyFileSystem;
use xui_core::app::{App, Ui};
use xui_core::backend::Result;
use xui_core::widget::{Button, FileDialog, HasText, Label};
use xui_core::{Key, Rect};
use xui_docs::{error_page, load_file};
use xui_litehtml::{HtmlView, HtmlViewEvent};

/// Height of the toolbar above the page, in pixels.
const TOOLBAR_H: i32 = 36;
/// Where the Open dialog starts when no document is open yet.
const START_DIR: &str = "/";

/// Everything the window reacts to.
pub enum Msg {
    /// litehtml finished a layout pass on its worker thread.
    Frame,
    /// Polls for the first painted frame, to print the render marker once.
    Tick,
    Link(String),
    Copy(String),
    /// The Open button or `Ctrl+O`.
    Open,
    /// The dialog accepted `path`.
    OpenChosen(PathBuf),
    /// The dialog was cancelled.
    DialogClosed,
}

/// A view of `html` filling `bounds`. One view per document: `HtmlView::load`
/// on a shown view keeps the painter's font cache, keyed by per-document font
/// ids, so the next document's fonts resolve to the previous one's entries
/// (small monospace headings, oversized italics). A fresh view starts clean.
fn make_view(ui: &Ui<Msg>, bounds: Rect, html: String) -> Result<HtmlView<Msg>> {
    HtmlView::new(
        ui,
        bounds,
        html,
        || Msg::Frame,
        |event| match event {
            HtmlViewEvent::LinkClicked(href) => Some(Msg::Link(href)),
            HtmlViewEvent::CopyRequested(text) => Some(Msg::Copy(text)),
        },
    )
}

/// The window's widgets and state.
pub struct Docs {
    view: HtmlView<Msg>,
    /// Where the page view sits (below the toolbar).
    view_bounds: Rect,
    path_label: Label<Msg>,
    open_dialog: FileDialog<Msg>,
    /// While the modal dialog is up the hotkey must not open a second one.
    dialog_open: Rc<Cell<bool>>,
    /// The document on show, for the dialog's start folder.
    current: Option<PathBuf>,
    reported: bool,
    // Held so the widget lives as long as the window.
    _open_button: Button<Msg>,
}

impl Docs {
    /// Builds the toolbar and the page view showing `html`; `path` is the
    /// document it came from, if any.
    pub fn build(ui: &mut Ui<Msg>, html: String, path: Option<PathBuf>) -> Result<Docs> {
        let client = ui.client_rect();
        // `Rect::new` is (left, top, right, bottom).
        let open_button =
            Button::new(ui, Rect::new(8, 5, 100, 31), "Open...")?.on_click(|| Some(Msg::Open));
        let path_label = Label::new(ui, Rect::new(110, 8, (client.width() - 8).max(110), 30), "")?;

        let dialog_open = Rc::new(Cell::new(false));
        let closed = Rc::clone(&dialog_open);
        let open_dialog = FileDialog::open_file(ui, "Open")?
            .file_system(LazyFileSystem::shared())
            .initial_dir(START_DIR)
            .filter("Markdown", &["md", "markdown"])
            .filter("All files", &[])
            .require_existing(true)
            .on_accept(|path| Some(Msg::OpenChosen(path)))
            .on_cancel(move || {
                closed.set(false);
                Some(Msg::DialogClosed)
            });

        let hotkey_gate = Rc::clone(&dialog_open);
        ui.on_key(move |key, modifiers| {
            let command = modifiers.ctrl || modifiers.win;
            (command && key == Key::O && !hotkey_gate.get()).then_some(Msg::Open)
        });

        let view_bounds = Rect::new(0, TOOLBAR_H, client.width(), client.height().max(TOOLBAR_H));
        let view = make_view(ui, view_bounds, html)?;
        let tick = ui.set_timer(100);
        ui.on_timer(move |id| (id == tick).then_some(Msg::Tick));

        let mut docs = Docs {
            view,
            view_bounds,
            path_label,
            open_dialog,
            dialog_open,
            current: None,
            reported: false,
            _open_button: open_button,
        };
        docs.show(ui, path);
        Ok(docs)
    }

    /// Records `path` as the document on show: toolbar text and window title.
    fn show(&mut self, ui: &mut Ui<Msg>, path: Option<PathBuf>) {
        match &path {
            Some(path) => {
                self.path_label.set_text(&path.display().to_string());
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into(),
                );
                ui.set_window_title(&format!("{name} - Docs"));
            }
            None => {
                self.path_label
                    .set_text("Welcome (Ctrl+O opens a document)");
                ui.set_window_title("Docs");
            }
        }
        self.current = path;
    }

    /// Shows the document at `path` in a fresh view, or the reason it cannot be
    /// read.
    pub fn open(&mut self, ui: &mut Ui<Msg>, path: PathBuf) {
        let (html, marker) = match load_file(&path) {
            Ok(html) => (html, "PASS"),
            Err(error) => (error_page(&format!("{}: {error}", path.display())), "FAIL"),
        };
        match make_view(ui, self.view_bounds, html) {
            // Assigning drops the old view, which destroys its node and stops
            // its worker thread.
            Ok(view) => self.view = view,
            Err(error) => println!("DOCS:VIEW:FAIL:{error}"),
        }
        self.show(ui, Some(path.clone()));
        println!("DOCS:OPEN:{marker}:{}", path.display());
    }

    /// Shows the Open dialog, starting in the current document's folder.
    fn ask_for_a_document(&mut self) {
        if self.dialog_open.replace(true) {
            return;
        }
        if let Some(dir) = self.current.as_deref().and_then(|p| p.parent()) {
            self.open_dialog.set_initial_dir(dir);
        }
        self.open_dialog.open();
    }
}

impl App for Docs {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Frame => self.view.invalidate(),
            Msg::Tick => {
                if !self.reported && self.view.is_ready() {
                    self.reported = true;
                    println!("DOCS:RENDER:PASS");
                }
            }
            Msg::Link(href) => println!("DOCS:LINK:{href}"),
            Msg::Copy(text) => {
                // `HtmlView` only reports the request; the app owns the clipboard.
                ui.set_clipboard_text(&text);
                println!("DOCS:COPY:{} bytes", text.len());
            }
            Msg::Open => self.ask_for_a_document(),
            Msg::OpenChosen(path) => {
                self.dialog_open.set(false);
                self.open(ui, path);
            }
            Msg::DialogClosed => self.dialog_open.set(false),
        }
    }
}
