//! The Docs window: a slim toolbar (an Open button and the current path) above
//! the Blitz page, and the Open dialog behind the button and `Ctrl+O`.
//!
//! Serial evidence (the screenshot sessions wait on it): `DOCS:RENDER:PASS` once
//! the welcome page's first frame has been painted, `DOCS:OPEN:PASS:<path>` after
//! a document loads and `DOCS:RENDER:PASS:<path>` once *its* view has painted (a
//! fresh view per document, so the first marker says nothing about it), `DOCS:OPEN:FAIL:<path>` when it cannot be read, and
//! `DOCS:LINK:<href>` on a link click.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::arrange::{absolute, button, label, Handle, LayoutExt};
use xui_core::backend::Result;
use xui_core::widget::StdFileSystem;
use xui_core::widget::{FileDialog, HasText, Label};
use xui_core::{Key, Px, Rect};
use xui_docs::{error_page, load_file, themed};
use xui_blitz::{BlitzView, BlitzViewEvent};

/// Height of the toolbar above the page, in pixels.
const TOOLBAR_H: i32 = 36;
/// Where the Open dialog starts when no document is open yet: the
/// documentation root, holding the OS docs (`/docs/os`) and, from F4, the
/// installed apps' docs (`/docs/apps`).
const START_DIR: &str = fhs::docs::DOCS_ROOT;

/// Everything the window reacts to.
pub enum Msg {
    /// The engine drew a new frame.
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

/// A view of `html` filling `bounds`. One view per document: a fresh view
/// starts clean, and dropping the old one stops its engine thread.
fn make_view(ui: &Ui<Msg>, bounds: Rect, html: String) -> Result<BlitzView<Msg>> {
    BlitzView::builder(|| Msg::Frame, |event| match event {
        BlitzViewEvent::LinkClicked(href) => Some(Msg::Link(href)),
        BlitzViewEvent::CopyRequested(text) => Some(Msg::Copy(text)),
        _ => None,
    })
    .html(themed(html, ui.theme().is_dark))
    .build(ui, bounds)
}

/// The window's widgets and state.
pub struct Docs {
    view: BlitzView<Msg>,
    /// Where the page view sits (below the toolbar).
    view_bounds: Rect,
    path_label: Rc<Label<Msg>>,
    open_dialog: FileDialog<Msg>,
    /// While the modal dialog is up the hotkey must not open a second one.
    dialog_open: Rc<Cell<bool>>,
    /// The document on show, for the dialog's start folder.
    current: Option<PathBuf>,
    /// What to append to `DOCS:RENDER:PASS` when the current view first paints
    /// (empty for the welcome page, `:<path>` for a document); `None` once
    /// reported.
    render_marker: Option<String>,
}

impl Docs {
    /// Builds the toolbar and the page view showing `html`; `path` is the
    /// document it came from, if any.
    pub fn build(ui: &mut Ui<Msg>, html: String, path: Option<PathBuf>) -> Result<Docs> {
        let client = ui.client_rect();
        // The toolbar is laid out in pixels, like the page view under it.
        let dpi = ui.dpi();
        let px = |value: i32| Px(value).to_dip(dpi);
        let path_label = Handle::new();
        ui.root(absolute().children(
            (
                button("Open...").on_click_with(|| Some(Msg::Open)).at(
                    px(8),
                    px(5),
                    px(92),
                    px(26),
                ),
                label("").bind(&path_label).at(
                    px(110),
                    px(8),
                    px((client.width() - 118).max(0)),
                    px(22),
                ),
            ),
        ))?;
        let path_label = path_label.get();

        let dialog_open = Rc::new(Cell::new(false));
        let closed = Rc::clone(&dialog_open);
        let open_dialog = FileDialog::open_file(ui, "Open")?
            .file_system(Rc::new(StdFileSystem))
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
            render_marker: Some(String::new()),
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
        self.render_marker = Some(format!(":{}", path.display()));
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
            Msg::Frame => self.view.update(),
            Msg::Tick => {
                if self.render_marker.is_some() && self.view.is_ready() {
                    let suffix = self.render_marker.take().unwrap_or_default();
                    println!("DOCS:RENDER:PASS{suffix}");
                }
            }
            Msg::Link(href) => println!("DOCS:LINK:{href}"),
            Msg::Copy(text) => {
                // `BlitzView` only reports the request; the app owns the clipboard.
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
