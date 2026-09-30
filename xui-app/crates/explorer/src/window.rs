#![forbid(unsafe_code)]

//! The per-window [`App`]: one open folder, its [`IconView`] and [`StatusBar`].
//!
//! Widgets map their events to [`Msg`] through closures fixed at construction;
//! every effect (open a window, show a menu or dialog, delete, refresh) runs in
//! [`App::update`], after any `RefCell` borrow has been released. Window state
//! lives in this struct, not in shared cells.

mod actions;
mod flash;

use std::cell::Cell;
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;

pub use flash::FlashHandle;

use xui_core::app::{App, Ui};
use xui_core::backend::{BackendError, Event, NodeKind, NodeSpec, TimerId, WidgetId};
use xui_core::geometry::{Point, Rect};
use xui_core::message::Key;
use xui_core::units::Dip;
use xui_core::widget::{
    Control, Dialog, IconView, Menu, MenuId, Placeable, StatusBar, TaskDialog, TaskDialogAction,
};

use crate::model::{Clock, Flash, Listing, SharedListing, summarize, title};
use crate::platform::Kind;
use crate::shell::Explorer;

/// The status bar's design height.
const STATUS_HEIGHT: Dip = Dip(24.0);

/// How often the open-folder flash is checked, in milliseconds. Short enough
/// that a folder reverts close to its two-second deadline.
const FLASH_TICK_MS: u32 = 200;

/// The context menu's command ids.
const MENU_OPEN: MenuId = MenuId::new(0);
const MENU_DELETE: MenuId = MenuId::new(1);
const MENU_PROPERTIES: MenuId = MenuId::new(2);
const MENU_REFRESH: MenuId = MenuId::new(3);

/// One explorer window's messages.
#[derive(Clone, Copy)]
pub enum Msg {
    /// The icon view's selection changed.
    Selection,
    /// An item was double-clicked or activated with Return.
    Activate(usize),
    /// The icon view was right-clicked: the item (or nothing on empty space)
    /// and the pointer position in node-local pixels.
    Context(Option<usize>, Point),
    /// A context-menu command was chosen.
    Menu(MenuId),
    /// Re-list the folder.
    Refresh,
    /// Delete the selection (opens the confirm dialog).
    Delete,
    /// Show the selection's properties.
    Properties,
    /// The delete confirmation was answered.
    Confirm(TaskDialogAction),
    /// The properties dialog was dismissed.
    PropertiesClosed,
    /// The open-folder flash's repeating timer fired.
    FlashTick,
    /// The window was resized; the view and status bar re-flow to the new
    /// client rect.
    WindowResized,
}

/// One open folder window.
pub struct ExplorerWindow {
    explorer: Rc<Explorer>,
    dir: PathBuf,
    title: String,
    listing: Rc<Listing>,
    flash: Rc<Flash>,
    view: Rc<IconView<Msg>>,
    status: Rc<StatusBar<Msg>>,
    menu: Menu<Msg>,
    /// A zero-size container that owns the flash's repeating timer; dropping
    /// the control (on window close) stops the timer.
    timer: Control<Msg>,
    /// The backend's id for the running tick timer, and whether one should be
    /// running. The flag is the single source of truth so a second timer is
    /// never started; the id is what `kill_timer` needs.
    timer_id: Rc<Cell<Option<TimerId>>>,
    ticking: Rc<Cell<bool>>,
    context_item: Option<usize>,
    pending_delete: Vec<OsString>,
    confirm: Option<TaskDialog<Msg>>,
    properties: Option<Dialog<Msg>>,
}

impl ExplorerWindow {
    /// Builds a window showing `dir`, wired to `explorer`'s platform and
    /// launcher. The listing is read immediately, on the UI thread. The
    /// open-folder flash uses the system clock.
    pub fn new(
        ui: &mut Ui<Msg>,
        explorer: Rc<Explorer>,
        dir: PathBuf,
    ) -> Result<ExplorerWindow, BackendError> {
        let clock: Clock = Rc::new(std::time::Instant::now);
        ExplorerWindow::with_clock(ui, explorer, dir, clock)
    }

    /// Builds a window whose open-folder flash reads `clock`; a test injects a
    /// clock it can advance.
    pub fn with_clock(
        ui: &mut Ui<Msg>,
        explorer: Rc<Explorer>,
        dir: PathBuf,
        clock: Clock,
    ) -> Result<ExplorerWindow, BackendError> {
        let listing = Rc::new(Listing::load(explorer.platform(), &dir));
        let flash = Rc::new(Flash::with_clock(clock));
        let timer = Control::new(ui, &NodeSpec::new(NodeKind::Container, Rect::default()))?;
        let (view_rect, status_rect) = layout(ui);
        let view = Rc::new(
            IconView::with_model(
                ui,
                view_rect,
                SharedListing::with_flash(Rc::clone(&listing), Rc::clone(&flash)),
            )?
            .multi_select(true)
            .on_selection(|_| Some(Msg::Selection))
            .on_activate(|index| Some(Msg::Activate(index)))
            .on_context(|item, at| Some(Msg::Context(item, at))),
        );
        let status = Rc::new(StatusBar::new(ui, status_rect, &[""])?);
        let menu = Menu::context(ui)
            .build(|scope| {
                scope.item(MENU_OPEN, "Open");
                scope.item(MENU_DELETE, "Delete");
                scope.item(MENU_PROPERTIES, "Properties");
                scope.separator();
                scope.item(MENU_REFRESH, "Refresh");
            })
            .on_select(|id| Some(Msg::Menu(id)));

        // Delete / Alt+Enter / F5 are claimed ahead of the focused widget.
        ui.on_key(|key, modifiers| match key {
            Key::DELETE => Some(Msg::Delete),
            Key::F5 => Some(Msg::Refresh),
            Key::RETURN if modifiers.alt => Some(Msg::Properties),
            _ => None,
        });

        // The compositor announces a resize at the window level; map it to a
        // message so the re-flow runs in `update`, outside event dispatch.
        ui.register_events(WidgetId::NONE, |event| match event {
            Event::Resize { .. } => Some(Msg::WindowResized),
            _ => None,
        });

        let mut window = ExplorerWindow {
            explorer,
            dir,
            title: String::new(),
            listing,
            flash,
            view,
            status,
            menu,
            timer,
            timer_id: Rc::new(Cell::new(None)),
            ticking: Rc::new(Cell::new(false)),
            context_item: None,
            pending_delete: Vec::new(),
            confirm: None,
            properties: None,
        };
        window.refresh(ui);
        Ok(window)
    }

    /// The window's current title (the folder's name, or the root's display
    /// form).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The status bar, shared so a test can read its parts after the app is
    /// built. The window owns the other reference.
    pub fn status_bar(&self) -> Rc<StatusBar<Msg>> {
        Rc::clone(&self.status)
    }

    /// The icon view, shared so a test can read or set the selection after the
    /// app is built. The window owns the other reference.
    pub fn view_handle(&self) -> Rc<IconView<Msg>> {
        Rc::clone(&self.view)
    }

    /// Re-lists the folder, keeps the selection where the items still exist,
    /// drops any flash whose folder vanished, and updates the title and status
    /// bar.
    fn refresh(&mut self, ui: &mut Ui<Msg>) {
        let selected_names = self.listing.names_of(&self.view.selection());
        let listing = Rc::new(Listing::load(self.explorer.platform(), &self.dir));
        self.listing = Rc::clone(&listing);
        self.prune_flash(&listing);
        self.view.set_model(SharedListing::with_flash(
            Rc::clone(&listing),
            Rc::clone(&self.flash),
        ));
        self.view
            .set_selection(&listing.indices_of(&selected_names));

        self.title = title(&self.dir);
        ui.set_window_title(&self.title);
        self.explorer.publish_title(ui.window(), &self.title);
        self.update_status();
    }

    /// Rewrites the status bar from the listing and the selection, or shows the
    /// read error when the folder could not be listed.
    fn update_status(&self) {
        match &self.listing.error {
            Some(error) => self.status.set_parts(&[error]),
            None => {
                let parts = summarize(&self.listing.entries, &self.view.selection());
                let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
                self.status.set_parts(&refs);
            }
        }
    }

    /// Re-lays the view and status bar out for the window's current client
    /// rect. The icon view owns a scrollbar child node, so it is told through
    /// [`Placeable::placed`] after the batch move; each open folder window
    /// re-flows on its own.
    fn reflow(&self, ui: &Ui<Msg>) {
        let (view_rect, status_rect) = layout(ui);
        ui.apply_moves(&[(self.view.id(), view_rect), (self.status.id(), status_rect)]);
        self.view.placed(ui, view_rect);
    }

    /// Opens a directory in its own window (or reports it already open) or
    /// hands a file to the launcher.
    fn activate(&mut self, index: usize, ui: &mut Ui<Msg>) {
        let Some(entry) = self.listing.entries.get(index).cloned() else {
            return;
        };
        let path = self.dir.join(&entry.name);
        match entry.kind {
            Kind::Dir => {
                // Purely visual: flag the tile open (even when the window is
                // already showing that folder) without touching selection or
                // opening behaviour.
                self.flash_name(&entry.name);
                if !self.explorer.open_or_reuse(ui, path) {
                    self.status.set_parts(&["already open"]);
                }
            }
            Kind::File | Kind::Symlink => {
                if let Err(error) = self.explorer.launcher().open(&path) {
                    self.status
                        .set_parts(&[&format!("Cannot open {}: {error}", entry.display)]);
                }
            }
        }
    }

    /// Shows the context menu, first making the right-clicked item the selection
    /// if it was not part of it, and disabling the item commands on empty space.
    fn context(&mut self, item: Option<usize>, at: Point, ui: &mut Ui<Msg>) {
        self.context_item = item;
        let has_item = item.is_some();
        self.menu.set_enabled(MENU_OPEN, has_item);
        self.menu.set_enabled(MENU_DELETE, has_item);
        self.menu.set_enabled(MENU_PROPERTIES, has_item);
        self.menu.set_enabled(MENU_REFRESH, true);
        // The right click may have changed the selection (XP selects the
        // unselected tile under the pointer), so refresh the status line.
        self.update_status();
        let bounds = ui.bounds(self.view.id());
        self.menu
            .show_context(bounds.left + at.x, bounds.top + at.y);
    }

    /// Whether a confirm or properties dialog is open. Shortcuts and the
    /// context menu are ignored while one is: the modal owns the window.
    fn modal_open(&self) -> bool {
        self.confirm.as_ref().is_some_and(TaskDialog::is_open)
            || self.properties.as_ref().is_some_and(Dialog::is_open)
    }

    /// Runs a context-menu command against the right-clicked item.
    fn menu_command(&mut self, id: MenuId, ui: &mut Ui<Msg>) {
        if id == MENU_OPEN {
            if let Some(index) = self.context_item {
                self.activate(index, ui);
            }
        } else if id == MENU_DELETE {
            self.begin_delete(ui);
        } else if id == MENU_PROPERTIES {
            self.show_properties(ui);
        } else if id == MENU_REFRESH {
            self.refresh(ui);
        }
    }
}

impl App for ExplorerWindow {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            // A resize re-flows even while a modal is open: the dialogs are
            // window-level nodes and keep their own layout.
            Msg::WindowResized => self.reflow(ui),
            Msg::Selection => self.update_status(),
            Msg::Activate(index) => {
                if !self.modal_open() {
                    self.activate(index, ui);
                }
            }
            Msg::Context(item, at) => {
                if !self.modal_open() {
                    self.context(item, at, ui);
                }
            }
            Msg::Menu(id) => {
                if !self.modal_open() {
                    self.menu_command(id, ui);
                }
            }
            Msg::Refresh => {
                if !self.modal_open() {
                    self.refresh(ui);
                }
            }
            Msg::Delete => {
                if !self.modal_open() {
                    self.begin_delete(ui);
                }
            }
            Msg::Properties => {
                if !self.modal_open() {
                    self.show_properties(ui);
                }
            }
            Msg::Confirm(action) => self.resolve_delete(action, ui),
            Msg::PropertiesClosed => self.properties = None,
            Msg::FlashTick => self.flash_tick(),
        }
    }
}

/// The icon view's and status bar's rectangles, in device pixels.
fn layout(ui: &Ui<Msg>) -> (Rect, Rect) {
    let client = ui.client_rect();
    let status_height = STATUS_HEIGHT.to_px(ui.dpi()).value();
    let status_top = (client.bottom - status_height).max(client.top);
    (
        Rect::new(client.left, client.top, client.right, status_top),
        Rect::new(client.left, status_top, client.right, client.bottom),
    )
}

#[cfg(test)]
mod tests;
