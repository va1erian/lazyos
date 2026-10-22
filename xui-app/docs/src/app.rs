//! The Docs window: a slim toolbar (Open, Back, Forward, Home and the current
//! path) above the Blitz page, and the Open dialog behind the button and `Ctrl+O`.
//! A click on a link to another Markdown document opens it (Back and Forward,
//! `Alt+Left` and `Alt+Right`, walk what was visited); `#heading` links scroll.
//!
//! Serial evidence (the screenshot sessions wait on it): `DOCS:RENDER:PASS` once
//! the welcome page's first frame has been painted, `DOCS:OPEN:PASS:<path>` after
//! a document loads and `DOCS:RENDER:PASS:<path>` once *its* view has painted (a
//! fresh view per document, so the first marker says nothing about it), `DOCS:OPEN:FAIL:<path>` when it cannot be read,
//! `DOCS:LINK:<href>` on a link click and `DOCS:NAV:PASS:<link|back|forward|home>:<path>`
//! when the click moved to a document.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::arrange::{absolute, button, label, Handle, LayoutExt};
use xui_core::backend::Result;
use xui_core::widget::StdFileSystem;
use xui_core::widget::{FileDialog, HasText, Label};
use xui_app::trace::{self, Channel};
use xui_core::{Key, Px, Rect};
use xui_docs::nav::{base_url, classify, History, Link, Place};
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
    Back,
    Forward,
    /// The documentation index.
    Home,
    /// The dialog accepted `path`.
    OpenChosen(PathBuf),
    /// The dialog was cancelled.
    DialogClosed,
}

/// A view of `html` filling `bounds`, laid out against `base` (the document's
/// own `file://` URL, so its relative links resolve and a `#fragment` scrolls).
/// One view per document: a fresh view starts clean, and dropping the old one
/// stops its engine thread.
fn make_view(ui: &Ui<Msg>, bounds: Rect, html: String, base: String) -> Result<BlitzView<Msg>> {
    BlitzView::builder(|| Msg::Frame, |event| match event {
        BlitzViewEvent::LinkClicked(href) => Some(Msg::Link(href)),
        BlitzViewEvent::CopyRequested(text) => Some(Msg::Copy(text)),
        _ => None,
    })
    .html(themed(html, ui.theme().is_dark))
    .base_url(base)
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
    /// The page on show as HTML (not yet themed), to scroll it to a heading.
    html: String,
    /// The welcome page, shown again by Back and by a start with no document.
    welcome: String,
    history: History,
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
                button("Back").on_click_with(|| Some(Msg::Back)).at(
                    px(108),
                    px(5),
                    px(56),
                    px(26),
                ),
                button("Forward").on_click_with(|| Some(Msg::Forward)).at(
                    px(168),
                    px(5),
                    px(72),
                    px(26),
                ),
                button("Home").on_click_with(|| Some(Msg::Home)).at(
                    px(244),
                    px(5),
                    px(56),
                    px(26),
                ),
                label("").bind(&path_label).at(
                    px(308),
                    px(8),
                    px((client.width() - 316).max(0)),
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
            if modifiers.alt && !hotkey_gate.get() {
                match key {
                    Key::LEFT => return Some(Msg::Back),
                    Key::RIGHT => return Some(Msg::Forward),
                    _ => {}
                }
            }
            (command && key == Key::O && !hotkey_gate.get()).then_some(Msg::Open)
        });

        let view_bounds = Rect::new(0, TOOLBAR_H, client.width(), client.height().max(TOOLBAR_H));
        let view = make_view(ui, view_bounds, html.clone(), base_url(None, None))?;
        let tick = ui.set_timer(100);
        ui.on_timer(move |id| (id == tick).then_some(Msg::Tick));

        let mut docs = Docs {
            view,
            view_bounds,
            path_label,
            open_dialog,
            dialog_open,
            current: None,
            welcome: html.clone(),
            html,
            history: History::default(),
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
    /// read. What was on show goes into the history.
    pub fn open(&mut self, ui: &mut Ui<Msg>, path: PathBuf) {
        self.history.visit(self.place());
        self.show_document(ui, path, None);
    }

    /// Where the window is now, for the history.
    fn place(&self) -> Place {
        Place {
            path: self.current.clone(),
        }
    }

    /// Replaces the view with the document at `path` (the welcome page for
    /// `None`).
    fn show_place(&mut self, ui: &mut Ui<Msg>, path: Option<PathBuf>) {
        let Some(path) = path else {
            let html = self.welcome.clone();
            self.replace_view(ui, html, base_url(None, None));
            self.render_marker = Some(String::new());
            self.show(ui, None);
            return;
        };
        self.show_document(ui, path, None);
    }

    /// Replaces the view with the document at `path`, scrolled to `fragment`
    /// when one is named.
    fn show_document(&mut self, ui: &mut Ui<Msg>, path: PathBuf, fragment: Option<&str>) {
        let (html, marker) = match load_file(&path) {
            Ok(html) => (html, "PASS"),
            Err(error) => (error_page(&format!("{}: {error}", path.display())), "FAIL"),
        };
        self.replace_view(ui, html, base_url(Some(&path), fragment));
        self.render_marker = Some(format!(":{}", path.display()));
        self.show(ui, Some(path.clone()));
        println!("DOCS:OPEN:{marker}:{}", path.display());
    }

    /// Swaps in a view of `html`; assigning drops the old view, which destroys
    /// its node and stops its worker thread.
    fn replace_view(&mut self, ui: &Ui<Msg>, html: String, base: String) {
        match make_view(ui, self.view_bounds, html.clone(), base) {
            Ok(view) => self.view = view,
            Err(error) => println!("DOCS:VIEW:FAIL:{error}"),
        }
        self.html = html;
    }

    /// Scrolls the page on show to `fragment`: the same page, laid out again
    /// against its URL with the fragment, which the engine scrolls to.
    fn scroll_to(&mut self, ui: &Ui<Msg>, fragment: &str) {
        let base = base_url(self.current.as_deref(), Some(fragment));
        let html = themed(self.html.clone(), ui.theme().is_dark);
        self.view.load_html(html, base);
    }

    /// A click on a link: another document opens, a heading of this one
    /// scrolls, anything else is ignored (no network, no other programs).
    fn follow(&mut self, ui: &mut Ui<Msg>, href: &str) {
        match classify(href) {
            Some(Link::Document { path, fragment }) => {
                if self.current.as_deref() == Some(path.as_path()) {
                    if let Some(fragment) = fragment {
                        self.scroll_to(ui, &fragment);
                    }
                    return;
                }
                self.history.visit(self.place());
                self.show_document(ui, path.clone(), fragment.as_deref());
                let shown = path.display().to_string();
                trace::pass!(Channel::Docs, "NAV", "link:{shown}");
            }
            Some(Link::Other(_) | Link::External(_)) | None => {}
        }
    }

    /// Back (`forward` false) or Forward through the documents visited.
    fn step(&mut self, ui: &mut Ui<Msg>, forward: bool) {
        let here = self.place();
        let target = if forward {
            self.history.forward(here)
        } else {
            self.history.back(here)
        };
        if let Some(place) = target {
            self.show_place(ui, place.path.clone());
            let name = if forward { "forward" } else { "back" };
            let path = place.path.map(|p| p.display().to_string()).unwrap_or_default();
            trace::pass!(Channel::Docs, "NAV", "{name}:{path}");
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
            Msg::Frame => self.view.update(),
            Msg::Tick => {
                if self.render_marker.is_some() && self.view.is_ready() {
                    let suffix = self.render_marker.take().unwrap_or_default();
                    println!("DOCS:RENDER:PASS{suffix}");
                }
            }
            Msg::Link(href) => {
                println!("DOCS:LINK:{href}");
                self.follow(ui, &href);
            }
            Msg::Back => self.step(ui, false),
            Msg::Forward => self.step(ui, true),
            Msg::Home => {
                let index = fhs::docs::INDEX;
                self.open(ui, PathBuf::from(index));
                trace::pass!(Channel::Docs, "NAV", "home:{index}");
            }
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
