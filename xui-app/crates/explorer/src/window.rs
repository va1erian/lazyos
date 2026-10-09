#![forbid(unsafe_code)]

//! The per-window [`App`]: an explorer window that browses folders in place.
//!
//! A window shows one folder at a time and keeps a browser-style
//! [`History`]: opening a folder (a double click, Return, the address bar, Up)
//! replaces the view, Back and Forward walk the visits, and the title is the
//! folder's name. The context menu's "Open in New Window" is the only way to
//! get a second window. The folder is shown as icon tiles or as a details
//! list with sortable Name, Size, Type and Modified columns; the toolbar
//! switches between them and picks the sort.
//!
//! Widgets map their events to [`Msg`] through closures fixed at construction;
//! every effect (navigate, open a window, show a menu or dialog, delete,
//! refresh) runs in [`App::update`], after any `RefCell` borrow has been
//! released. Window state lives in this struct, not in shared cells.

mod actions;
mod backdrop;
mod chrome;
mod clipboard;
mod keys;
mod nav;
mod view;

use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use xui_core::app::{App, Proxy, Ui};
use xui_core::backend::BackendError;
use xui_core::geometry::Point;
use xui_core::message::{Key, Modifiers};
use xui_core::widget::{
    Dialog, Edit, IconView, ListView, Menu, MenuId, StatusBar, TaskDialog, TaskDialogAction,
};

use self::chrome::{Chrome, Handles};
pub use self::chrome::{SORT_DESCENDING, sort_id as sort_menu_id};
use crate::model::{History, Listing, SharedListing, SortOrder};
use crate::shell::Explorer;

/// How a window shows its folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// Icon tiles that flow and wrap.
    #[default]
    Icons,
    /// One row per entry under sortable columns.
    Details,
}

/// What a window starts with; "Open in New Window" passes the opener's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewOptions {
    /// Icons or details.
    pub mode: ViewMode,
    /// The listing order.
    pub sort: SortOrder,
}

/// One explorer window's messages.
#[derive(Clone, Copy, Debug)]
pub enum Msg {
    /// The view's selection changed.
    Selection,
    /// An item was double-clicked or activated with Return.
    Activate(usize),
    /// Open a folder entry in a new window (the context menu).
    OpenInNewWindow(usize),
    /// A view was right-clicked: the item (or nothing on empty space) and the
    /// pointer position in node-local pixels.
    Context(Option<usize>, Point),
    /// A context-menu command was chosen.
    Menu(MenuId),
    /// Re-list the folder.
    Refresh,
    /// Delete the selection (opens the confirm dialog).
    Delete,
    /// Show the selection's properties.
    Properties,
    /// Show the properties of the folder the window shows.
    FolderProperties,
    /// Put the selection on the clipboard (Ctrl+C).
    Copy,
    /// Copy the clipboard's files into this folder (Ctrl+V).
    Paste,
    /// The delete confirmation was answered.
    Confirm(TaskDialogAction),
    /// The properties dialog was dismissed.
    PropertiesClosed,
    /// A drag out of the view started carrying the selection the press
    /// collapsed: select it again.
    RestoreSelection,
    /// Go to the previous folder.
    Back,
    /// Go to the next folder.
    Forward,
    /// Go to the parent folder.
    Up,
    /// The address bar's text was edited.
    AddressEdited,
    /// Open what the address bar holds.
    Go,
    /// Put the current folder back in the address bar.
    RestoreAddress,
    /// Move the keyboard to an emptied address bar (`Edit` has no public
    /// select-all, so the old path is cleared, as LazyWeb does; Escape puts
    /// it back).
    FocusAddress,
    /// A key the window watches; [`keys::shortcut`] decides what it means.
    Key(Key, Modifiers),
    /// The Sort button was clicked: show the sort menu.
    SortMenu,
    /// A sort menu item was chosen.
    SortChosen(MenuId),
    /// A details column header was clicked.
    SortColumn(usize),
    /// Switch between the icon and the details view.
    ToggleView,
    /// A click that hit no control (empty toolbar space, the status bar):
    /// it only closes the menus.
    Dismiss,
}

/// One explorer window.
pub struct ExplorerWindow {
    explorer: Rc<Explorer>,
    dir: PathBuf,
    title: String,
    history: History,
    options: ViewOptions,
    listing: Rc<Listing>,
    chrome: Chrome,
    menu: Menu<Msg>,
    sort_menu: Menu<Msg>,
    context_item: Option<usize>,
    pending_delete: Vec<OsString>,
    confirm: Option<TaskDialog<Msg>>,
    properties: Option<Dialog<Msg>>,
    /// Whether the address bar holds an edit not yet submitted.
    address_dirty: bool,
    /// The selection an Alt+arrow navigation left, and when: see
    /// [`keys::ARROW_ECHO`].
    arrow_guard: Option<(Instant, Vec<usize>)>,
    /// This window's raw id, under which its view state is published.
    window: u64,
    /// Reaches this window from the platform's drag hooks.
    proxy: Proxy<Msg>,
    /// The selection, and the one before its latest change.
    selected: Vec<usize>,
    previous: Vec<usize>,
}

impl ExplorerWindow {
    /// Builds a window showing `dir` as icons sorted by name, wired to
    /// `explorer`'s platform and launcher. The listing is read immediately,
    /// on the UI thread.
    pub fn new(
        ui: &mut Ui<Msg>,
        explorer: Rc<Explorer>,
        dir: PathBuf,
    ) -> Result<ExplorerWindow, BackendError> {
        ExplorerWindow::with_options(ui, explorer, dir, ViewOptions::default())
    }

    /// Builds a window showing `dir` in the view and order `options` give.
    pub fn with_options(
        ui: &mut Ui<Msg>,
        explorer: Rc<Explorer>,
        dir: PathBuf,
        options: ViewOptions,
    ) -> Result<ExplorerWindow, BackendError> {
        let listing = Rc::new(Listing::load(explorer.platform(), &dir, options.sort));
        let handles = Handles::default();
        ui.root(handles.layout(SharedListing::new(Rc::clone(&listing))))?;
        let chrome = handles.get();
        ui.register_events(chrome.status.id(), backdrop::dismiss);
        ui.on_key(|key, modifiers| {
            keys::watched(key, modifiers).then_some(Msg::Key(key, modifiers))
        });
        let mut window = ExplorerWindow {
            menu: chrome::context_menu(ui),
            sort_menu: chrome::sort_menu(ui),
            explorer,
            dir,
            title: String::new(),
            history: History::new(),
            options,
            listing,
            chrome,
            context_item: None,
            pending_delete: Vec::new(),
            confirm: None,
            properties: None,
            address_dirty: false,
            arrow_guard: None,
            window: ui.window().raw(),
            proxy: ui.proxy(),
            selected: Vec::new(),
            previous: Vec::new(),
        };
        window.apply_mode(ui);
        window.show_listing(ui, Rc::clone(&window.listing), &[]);
        Ok(window)
    }

    /// The window's current title: the folder's name, or the root's display
    /// form.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The folder the window shows.
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// The view and order the window uses.
    pub fn options(&self) -> ViewOptions {
        self.options
    }

    /// The status bar, shared so a test can read its parts after the app is
    /// built. The window owns the other reference.
    pub fn status_bar(&self) -> Rc<StatusBar<Msg>> {
        Rc::clone(&self.chrome.status)
    }

    /// The icon view, shared so a test can read or set its selection.
    pub fn view_handle(&self) -> Rc<IconView<Msg>> {
        Rc::clone(&self.chrome.icons)
    }

    /// The details list, shared so a test can read its rows and selection.
    pub fn details_handle(&self) -> Rc<ListView<Msg>> {
        Rc::clone(&self.chrome.details)
    }

    /// The sort menu's popup node, so a test can see whether it is shown.
    pub fn sort_menu_popup(&self) -> Option<xui_core::backend::WidgetId> {
        self.sort_menu.popup_id(0)
    }

    /// The address bar, shared so a test can type into it.
    pub fn address_handle(&self) -> Rc<Edit<Msg>> {
        Rc::clone(&self.chrome.address)
    }

    /// Whether a confirm or properties dialog is open. Shortcuts and the
    /// context menu are ignored while one is: the modal owns the window.
    fn modal_open(&self) -> bool {
        self.confirm.as_ref().is_some_and(TaskDialog::is_open)
            || self.properties.as_ref().is_some_and(Dialog::is_open)
    }

    /// Whether the address bar has the keyboard: the backend's answer when
    /// it has one, else whether the user is editing it.
    fn address_focused(&self) -> bool {
        self.explorer
            .has_focus(self.chrome.address.id())
            .unwrap_or(self.address_dirty)
    }

    /// Runs a message that a modal dialog blocks.
    fn command(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Activate(index) => self.activate(index, ui),
            Msg::OpenInNewWindow(index) => self.open_in_new_window(index, ui),
            Msg::Context(item, at) => self.context(item, at, ui),
            Msg::Menu(id) => self.menu_command(id, ui),
            Msg::Refresh => self.refresh(ui),
            Msg::Delete => self.begin_delete(ui),
            Msg::Properties => self.show_selection_properties(ui),
            Msg::FolderProperties => self.show_folder_properties(ui),
            Msg::Copy => self.copy_selection(),
            Msg::Paste => self.paste(ui),
            Msg::Back => self.back(ui),
            Msg::Forward => self.forward(ui),
            Msg::Up => self.up(ui),
            Msg::Go => self.go_address(ui),
            Msg::RestoreAddress => self.restore_address(),
            Msg::FocusAddress => self.focus_address(),
            Msg::Key(key, modifiers) => {
                if let Some(msg) = keys::shortcut(key, modifiers, self.address_focused()) {
                    self.command(msg, ui);
                    if keys::is_arrow(key) {
                        self.arrow_guard = Some((Instant::now(), self.selection()));
                    }
                }
            }
            Msg::SortMenu => self.show_sort_menu(ui),
            Msg::SortChosen(id) => self.sort_chosen(id, ui),
            Msg::SortColumn(column) => self.sort_column(column, ui),
            Msg::ToggleView => self.toggle_view(ui),
            Msg::Selection
            | Msg::Confirm(_)
            | Msg::PropertiesClosed
            | Msg::RestoreSelection
            | Msg::AddressEdited
            | Msg::Dismiss => {}
        }
    }
}

impl App for ExplorerWindow {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        // xui's in-window menus close only on a choice or Escape, so any
        // other interaction that reaches the window dismisses them.
        let sort_was_open = self.sort_menu.is_open();
        if !matches!(msg, Msg::Menu(_) | Msg::SortChosen(_)) {
            self.menu.close();
            self.sort_menu.close();
        }
        match msg {
            // A second click on Sort closes its menu.
            Msg::SortMenu if sort_was_open => {}
            Msg::Selection => {
                // The view's own move for the arrow of an Alt+arrow shortcut
                // just handled: keep the selection the navigation made.
                if let Some((at, rows)) = self.arrow_guard.take()
                    && at.elapsed() < keys::ARROW_ECHO
                {
                    self.set_selection(&rows);
                }
                self.selection_changed();
            }
            Msg::RestoreSelection => {
                let previous = self.previous.clone();
                self.set_selection(&previous);
                self.selection_changed();
            }
            Msg::AddressEdited => self.address_dirty = true,
            Msg::Confirm(action) => self.resolve_delete(action, ui),
            Msg::PropertiesClosed => self.properties = None,
            other => {
                if !self.modal_open() {
                    self.command(other, ui);
                }
            }
        }
    }
}

impl Drop for ExplorerWindow {
    fn drop(&mut self) {
        self.explorer.forget_view(self.window);
    }
}

#[cfg(test)]
mod tests;
