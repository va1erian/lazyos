//! The Docs window: a slim toolbar (an Open button and the current path) above the page, and the
//! Open dialog behind the button and `Ctrl+O`. The page is a light view of `webd` (WebKit): one view
//! for the whole life of the window, every document a new page in it.
//!
//! Serial evidence (the screenshot sessions wait on it): `DOCS:RENDER:PASS` once the welcome page's
//! first frame has been painted, `DOCS:OPEN:PASS:<path>` after a document loads and
//! `DOCS:RENDER:PASS:<path>` once *its* page has painted (the first marker says nothing about it),
//! `DOCS:OPEN:FAIL:<path>` when it cannot be read, `DOCS:LINK:<href>` on a link click (then
//! `DOCS:LINK:OPEN:<path>`, `DOCS:LINK:URL:<url>` or `DOCS:LINK:REFUSED:<href>` for what was done with
//! it) and `DOCS:ENGINE:UP` / `DOCS:ENGINE:FAIL:<why>` when the web engine comes and goes.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use xui_app::lightview::{LightEvent, LightView};
use xui_app::platform::launcher;
use xui_app::trace::{self, Channel};
use xui_core::app::{App, Ui};
use xui_core::arrange::{build, button, column, label, row, Align, Handle, LayoutExt};
use xui_core::backend::Result;
use xui_core::layout::Insets;
use xui_core::widget::{FileDialog, HasText, Label, StdFileSystem};
use xui_core::{Color, Dip, Key, Rect};
use xui_docs::links::{classify, Link};
use xui_docs::{error_page, load_file, themed};
use xui_webd_client::Status;

/// Height of the toolbar above the page, in design units.
const TOOLBAR_H: f32 = 36.0;
/// Where the Open dialog starts when no document is open yet: the
/// documentation root, holding the OS docs (`/docs/os`) and, from F4, the
/// installed apps' docs (`/docs/apps`).
const START_DIR: &str = fhs::docs::DOCS_ROOT;
/// The page's own background, so the view matches it before the first picture ([`xui_docs`]'s CSS).
const LIGHT_PAGE: u32 = 0xff_ff_ff;
const DARK_PAGE: u32 = 0x23_2a_40;

/// Everything the window reacts to.
#[derive(Clone)]
pub enum Msg {
    /// The page was clicked on a link.
    Link(String),
    /// The page set last has been drawn.
    Painted,
    /// The web engine came up, or went away.
    Engine(Status),
    /// The Open button or `Ctrl+O`.
    Open,
    /// The dialog accepted `path`.
    OpenChosen(PathBuf),
    /// The dialog was cancelled.
    DialogClosed,
}

/// The window's widgets and state.
pub struct Docs {
    view: Rc<LightView<Msg>>,
    path_label: Rc<Label<Msg>>,
    open_dialog: FileDialog<Msg>,
    /// While the modal dialog is up the hotkey must not open a second one.
    dialog_open: Rc<Cell<bool>>,
    /// The document on show, for the dialog's start folder and for relative links.
    current: Option<PathBuf>,
    /// What follows `DOCS:RENDER:PASS:` when the current page first paints (empty for the welcome
    /// page, the path for a document); `None` once reported.
    render_marker: Option<String>,
    dark: bool,
}

impl Docs {
    /// Builds the toolbar and the page view showing `html`; `path` is the document it came from, if
    /// any.
    pub fn build(ui: &mut Ui<Msg>, html: String, path: Option<PathBuf>) -> Result<Docs> {
        let dark = ui.theme().is_dark;
        let page_colour = Color::hex(if dark { DARK_PAGE } else { LIGHT_PAGE });
        let path_label = Handle::new();
        let view: Handle<LightView<Msg>> = Handle::new();
        ui.root(column().children((
            row()
                .gap(8)
                .align(Align::Center)
                .padding(Insets::symmetric(Dip(8.0), Dip(5.0)))
                .children((
                    button("Open...").on_click(Msg::Open).width(Dip(92.0)),
                    label("").bind(&path_label).align_y(Align::Center).fill(1),
                ))
                .fixed(Dip(TOOLBAR_H)),
            build(move |ui: &Ui<Msg>| {
                LightView::new(ui, Rect::default(), page_colour, |event| match event {
                    LightEvent::Link(href) => Some(Msg::Link(href)),
                    LightEvent::Painted => Some(Msg::Painted),
                    LightEvent::Status(status) => Some(Msg::Engine(status)),
                })
            })
            .bind(&view)
            .fill(1),
        )))?;
        let path_label = path_label.get();
        let view = view.get();

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

        view.set_html(themed(html, dark));
        // Arrow keys and `PageDown` scroll the page from the start, no click needed.
        view.focus();
        let mut docs = Docs {
            view,
            path_label,
            open_dialog,
            dialog_open,
            current: None,
            render_marker: Some(String::new()),
            dark,
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

    /// Shows the document at `path` in the page, or the reason it cannot be read.
    pub fn open(&mut self, ui: &mut Ui<Msg>, path: PathBuf) {
        let shown = path.display();
        let (html, opened) = match load_file(&path) {
            Ok(html) => (html, true),
            Err(error) => (error_page(&format!("{shown}: {error}")), false),
        };
        self.view.set_html(themed(html, self.dark));
        self.render_marker = Some(shown.to_string());
        if opened {
            trace::pass!(Channel::Docs, "OPEN", "{shown}");
        } else {
            trace::fail!(Channel::Docs, "OPEN", "{shown}");
        }
        self.show(ui, Some(path.clone()));
    }

    /// What a click on `href` does (`xui_docs::links`).
    fn follow(&mut self, ui: &mut Ui<Msg>, href: &str) {
        trace::line!(Channel::Docs, "LINK:{href}");
        match classify(href, self.current.as_deref()) {
            Link::Anchor => {}
            Link::Document(path) => {
                trace::line!(Channel::Docs, "LINK:OPEN:{}", path.display());
                self.open(ui, path);
            }
            Link::File(path) => {
                trace::line!(Channel::Docs, "LINK:FILE:{}", path.display());
                if let Err(error) = launcher::open_path(&path.display().to_string()) {
                    trace::fail!(Channel::Docs, "LINK", "{}: {error}", path.display());
                }
            }
            Link::Url(url) => {
                trace::line!(Channel::Docs, "LINK:URL:{url}");
                if let Err(error) = launcher::open_url(&url) {
                    trace::fail!(Channel::Docs, "LINK", "{url}: {error}");
                }
            }
            Link::Refused => trace::line!(Channel::Docs, "LINK:REFUSED:{href}"),
        }
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
            Msg::Painted => {
                match self.render_marker.take() {
                    Some(path) if path.is_empty() => trace::pass!(Channel::Docs, "RENDER"),
                    Some(path) => trace::pass!(Channel::Docs, "RENDER", "{path}"),
                    None => {}
                }
            }
            Msg::Link(href) => self.follow(ui, &href),
            Msg::Engine(Status::Live) => trace::line!(Channel::Docs, "ENGINE:UP"),
            Msg::Engine(Status::NotRunning) => {
                trace::fail!(Channel::Docs, "ENGINE", "not running");
            }
            Msg::Engine(Status::Failed(why)) => trace::fail!(Channel::Docs, "ENGINE", "{why}"),
            Msg::Engine(Status::Starting) => {}
            Msg::Open => self.ask_for_a_document(),
            Msg::OpenChosen(path) => {
                self.dialog_open.set(false);
                self.open(ui, path);
            }
            Msg::DialogClosed => self.dialog_open.set(false),
        }
    }
}
