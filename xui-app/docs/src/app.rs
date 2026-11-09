//! The Docs window: a slim toolbar (Open, Back, Forward, Home and the current path) above the page,
//! and the Open dialog behind the button and `Ctrl+O`. The page is xui's read-only Markdown view
//! (`xui_markdown::MarkdownView`): one view for the life of the window, every document a new
//! [`Document`] in it, read and parsed by one worker thread ([`crate::loader`]) so a big file never
//! stalls the window.
//! Back and Forward (`Alt+Left`, `Alt+Right`) walk the documents visited, Home shows the
//! documentation index.
//!
//! Serial evidence (the screenshot sessions wait on it): `DOCS:RENDER:PASS` once the welcome page's
//! first frame has been painted, `DOCS:OPEN:PASS:<path>` after a document loads and
//! `DOCS:RENDER:PASS:<path>` once *its* page has painted (the first marker says nothing about it),
//! `DOCS:OPEN:FAIL:<path>` when it cannot be read, `DOCS:LINK:<href>` on a link click (then
//! `DOCS:LINK:OPEN:<path>`, `DOCS:LINK:URL:<url>` or `DOCS:LINK:REFUSED:<href>` for what was done with
//! it) and `DOCS:NAV:PASS:<back|forward|home>:<path>` when Back, Forward or Home moved to a page.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use xui_app::markdown::{style, RhaiCode};
use xui_app::platform::launcher;
use xui_app::trace::{self, Channel};
use xui_core::app::{App, Ui};
use xui_core::arrange::{build, button, column, label, row, Align, Handle, LayoutExt};
use xui_core::backend::Result;
use xui_core::layout::Insets;
use xui_core::widget::{FileDialog, HasText, Label, StdFileSystem};
use xui_core::{Dip, Key, Rect};
use xui_docs::error_page;
use xui_docs::history::{History, Place};
use xui_docs::links::{classify, Link};
use xui_markdown::{Document, MarkdownView};

use crate::loader::{Done, Loader, Request};

/// Height of the toolbar above the page, in design units.
const TOOLBAR_H: f32 = 36.0;
/// Where the Open dialog starts when no document is open yet: the
/// documentation root, holding the OS docs (`/docs/os`) and, from F4, the
/// installed apps' docs (`/docs/apps`).
const START_DIR: &str = fhs::docs::DOCS_ROOT;

/// Everything the window reacts to.
#[derive(Clone)]
pub enum Msg {
    /// The page was clicked on a link to somewhere else than one of its headings.
    Link(String),
    /// The document set last has been drawn.
    Painted,
    /// The worker finished reading a document.
    Loaded(Arc<Done>),
    /// The Open button or `Ctrl+O`.
    Open,
    /// The dialog accepted `path`.
    OpenChosen(PathBuf),
    /// The dialog was cancelled.
    DialogClosed,
    /// The Back button or `Alt+Left`.
    Back,
    /// The Forward button or `Alt+Right`.
    Forward,
    /// The Home button: the documentation index.
    Home,
}

/// The window's widgets and state.
pub struct Docs {
    view: Rc<MarkdownView<Msg>>,
    path_label: Rc<Label<Msg>>,
    open_dialog: FileDialog<Msg>,
    /// While the modal dialog is up the hotkey must not open a second one.
    dialog_open: Rc<Cell<bool>>,
    /// Where the window is going: the document on show, or the one being read for it (`None` is
    /// the welcome page). The history records it.
    current: Option<PathBuf>,
    /// The document on show, for the dialog's start folder and for relative links: until a read
    /// finishes, the page and its links are still the previous one's.
    shown: Option<PathBuf>,
    /// What follows `DOCS:RENDER:PASS:` when the current page first paints (empty for the welcome
    /// page, the path for a document); `None` once reported.
    render_marker: Option<String>,
    /// The welcome page, shown again by Back.
    welcome: Arc<Document>,
    history: History,
    /// The number of the last load started: an earlier one that finishes after it is dropped.
    loads: u64,
    /// The worker that reads documents; `None` if it could not be started.
    loader: Option<Loader>,
}

impl Docs {
    /// Builds the toolbar and the page view showing `welcome`.
    pub fn build(ui: &mut Ui<Msg>, welcome: &str) -> Result<Docs> {
        let path_label = Handle::new();
        let view: Handle<MarkdownView<Msg>> = Handle::new();
        ui.root(
            column().children((
                row()
                    .gap(8)
                    .align(Align::Center)
                    .padding(Insets::symmetric(Dip(8.0), Dip(5.0)))
                    .children((
                        button("Open...").on_click(Msg::Open).width(Dip(92.0)),
                        button("Back").on_click(Msg::Back).width(Dip(56.0)),
                        button("Forward").on_click(Msg::Forward).width(Dip(72.0)),
                        button("Home").on_click(Msg::Home).width(Dip(56.0)),
                        label("").bind(&path_label).align_y(Align::Center).fill(1),
                    ))
                    .fixed(Dip(TOOLBAR_H)),
                build(|ui: &Ui<Msg>| {
                    Ok(MarkdownView::new(ui, Rect::default())?
                        .style(style())
                        .highlighter(RhaiCode)
                        .on_link(|href| Some(Msg::Link(href.to_owned())))
                        .on_painted(|| Some(Msg::Painted)))
                })
                .bind(&view)
                .fill(1),
            )),
        )?;
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
            if hotkey_gate.get() {
                return None;
            }
            if modifiers.alt {
                match key {
                    Key::LEFT => return Some(Msg::Back),
                    Key::RIGHT => return Some(Msg::Forward),
                    _ => {}
                }
            }
            let command = modifiers.ctrl || modifiers.win;
            (command && key == Key::O).then_some(Msg::Open)
        });

        // Small and built in: parsed here.
        let welcome = Arc::new(Document::parse(welcome));
        view.set_document(Arc::clone(&welcome));
        // Arrow keys and `PageDown` scroll the page from the start, no click needed.
        view.focus();
        let loader = Loader::start(ui.proxy())
            .map_err(|error| trace::fail!(Channel::Docs, "OPEN", "no worker: {error}"))
            .ok();
        let mut docs = Docs {
            view,
            path_label,
            open_dialog,
            dialog_open,
            current: None,
            shown: None,
            render_marker: Some(String::new()),
            welcome,
            history: History::default(),
            loads: 0,
            loader,
        };
        docs.show(ui, None);
        Ok(docs)
    }

    /// Records `path` as the document on show: toolbar text, window title and the base of links.
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
        self.shown = path;
    }

    /// Shows the document at `path`, scrolled to the heading `anchor` names, or the reason it cannot
    /// be read. What was on show goes into the history, unless it is the same document: that one
    /// only scrolls to the heading, or is read again (it may have changed, or failed before).
    pub fn open(&mut self, path: PathBuf, anchor: Option<String>) {
        if self.current.as_deref() == Some(path.as_path()) {
            if let Some(anchor) = &anchor {
                if self.view.scroll_to_anchor(anchor) {
                    return;
                }
            }
        } else {
            self.history.visit(self.place());
        }
        self.load(path, anchor, None);
    }

    /// Where the window is now, for the history.
    fn place(&self) -> Place {
        Place {
            path: self.current.clone(),
        }
    }

    /// Asks the worker for the document at `path` (no history); [`Msg::Loaded`] shows it. The window
    /// is headed there at once, so Back and Forward step from it.
    fn load(&mut self, path: PathBuf, anchor: Option<String>, nav: Option<&'static str>) {
        self.loads += 1;
        self.current = Some(path.clone());
        let request = Request {
            seq: self.loads,
            path,
            anchor,
            nav,
        };
        let shown = request.path.display().to_string();
        if !self
            .loader
            .as_ref()
            .is_some_and(|loader| loader.load(request))
        {
            trace::fail!(Channel::Docs, "OPEN", "{shown}: no worker");
        }
    }

    /// Shows what the worker read, if it is still the newest load.
    fn loaded(&mut self, ui: &mut Ui<Msg>, done: &Done) {
        let request = &done.request;
        if request.seq != self.loads {
            return;
        }
        let shown = request.path.display();
        match &done.result {
            Ok(document) => {
                self.view.set_document(Arc::clone(document));
                if let Some(anchor) = &request.anchor {
                    self.view.scroll_to_anchor(anchor);
                }
                trace::pass!(Channel::Docs, "OPEN", "{shown}");
                if let Some(nav) = request.nav {
                    trace::pass!(Channel::Docs, "NAV", "{nav}:{shown}");
                }
            }
            Err(error) => {
                self.view
                    .set_markdown(&error_page(&format!("{shown}: {error}")));
                trace::fail!(Channel::Docs, "OPEN", "{shown}");
            }
        }
        self.render_marker = Some(shown.to_string());
        self.show(ui, Some(request.path.clone()));
    }

    /// Shows the welcome page again (no history).
    fn load_welcome(&mut self, ui: &mut Ui<Msg>, nav: &str) {
        // A document still being read must not replace it.
        self.loads += 1;
        self.view.set_document(Arc::clone(&self.welcome));
        self.render_marker = Some(String::new());
        self.current = None;
        self.show(ui, None);
        trace::pass!(Channel::Docs, "NAV", "{nav}:");
    }

    /// Back (`forward` false) or Forward through the documents visited.
    fn step(&mut self, ui: &mut Ui<Msg>, forward: bool) {
        let here = self.place();
        let target = if forward {
            self.history.forward(here)
        } else {
            self.history.back(here)
        };
        let Some(place) = target else { return };
        let nav = if forward { "forward" } else { "back" };
        match place.path {
            Some(path) => self.load(path, None, Some(nav)),
            None => self.load_welcome(ui, nav),
        }
    }

    /// What a click on `href` does (`xui_docs::links`).
    fn follow(&mut self, href: &str) {
        trace::line!(Channel::Docs, "LINK:{href}");
        match classify(href, self.shown.as_deref()) {
            Link::Anchor => {}
            Link::Document(path, anchor) => {
                trace::line!(Channel::Docs, "LINK:OPEN:{}", path.display());
                self.open(path, anchor);
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
        if let Some(dir) = self.shown.as_deref().and_then(|p| p.parent()) {
            self.open_dialog.set_initial_dir(dir);
        }
        self.open_dialog.open();
    }
}

impl App for Docs {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Painted => match self.render_marker.take() {
                Some(path) if path.is_empty() => trace::pass!(Channel::Docs, "RENDER"),
                Some(path) => trace::pass!(Channel::Docs, "RENDER", "{path}"),
                None => {}
            },
            Msg::Link(href) => self.follow(&href),
            Msg::Loaded(done) => self.loaded(ui, &done),
            Msg::Open => self.ask_for_a_document(),
            Msg::OpenChosen(path) => {
                self.dialog_open.set(false);
                self.open(path, None);
            }
            Msg::DialogClosed => self.dialog_open.set(false),
            Msg::Back => self.step(ui, false),
            Msg::Forward => self.step(ui, true),
            Msg::Home => {
                let index = PathBuf::from(fhs::docs::INDEX);
                if self.current.as_deref() != Some(index.as_path()) {
                    self.history.visit(self.place());
                }
                self.load(index, None, Some("home"));
            }
        }
    }
}
