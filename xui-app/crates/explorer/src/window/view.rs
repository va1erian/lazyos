#![forbid(unsafe_code)]

//! The two views of the folder (icon tiles and the details list) behind one
//! selection, the status bar, the view switch and sorting.
//!
//! Both views share one [`SharedListing`]; only the one on screen is asked
//! for its selection, and switching views carries the selection across.

use std::path::PathBuf;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::backend::WidgetId;
use xui_core::icon::Lucide;
use xui_core::widget::{MenuId, SortDirection};

use super::chrome::{SORT_DESCENDING, sort_id, sort_key_of};
use super::{ExplorerWindow, Msg, ViewMode};
use crate::model::{SharedListing, SortKey, SortOrder, summarize};
use crate::shell::ViewState;

impl ExplorerWindow {
    /// The view on screen.
    pub(super) fn active_view(&self) -> WidgetId {
        match self.options.mode {
            ViewMode::Icons => self.chrome.icons.id(),
            ViewMode::Details => self.chrome.details.id(),
        }
    }

    /// The selection of the view on screen, ascending.
    pub(super) fn selection(&self) -> Vec<usize> {
        match self.options.mode {
            ViewMode::Icons => self.chrome.icons.selection(),
            ViewMode::Details => self.chrome.details.selection(),
        }
    }

    /// Makes `rows` the selection of the view on screen, raising no event.
    pub(super) fn set_selection(&self, rows: &[usize]) {
        match self.options.mode {
            ViewMode::Icons => self.chrome.icons.set_selection(rows),
            ViewMode::Details => self.chrome.details.set_selection(rows),
        }
    }

    /// Scrolls `row` into view on screen.
    pub(super) fn ensure_visible(&self, row: usize) {
        match self.options.mode {
            ViewMode::Icons => self.chrome.icons.ensure_visible(row),
            ViewMode::Details => self.chrome.details.ensure_visible(row),
        }
    }

    /// Hands both views the current listing.
    ///
    /// The icon view takes its model while hidden: its `set_model` lays its
    /// scrollbar out holding its state borrowed, and when the scrollbar
    /// appears or goes the backend's synchronous `Resize` re-enters the
    /// layout, whose `placed` borrows that state mutably and panics. A hidden
    /// view is not placed; showing it again re-flows it once the message is
    /// done, outside the borrow.
    pub(super) fn set_models(&self, ui: &Ui<Msg>) {
        let model = SharedListing::new(Rc::clone(&self.listing));
        let icons = self.chrome.icons.id();
        let shown = ui.is_visible(icons);
        ui.set_visible(icons, false);
        self.chrome.icons.set_model(model.clone());
        ui.set_visible(icons, shown);
        self.chrome.details.set_model(model);
    }

    /// Shows the view the options name, hides the other, gives it the
    /// keyboard, and points the view switch at the other mode.
    pub(super) fn apply_mode(&mut self, ui: &mut Ui<Msg>) {
        let details = self.options.mode == ViewMode::Details;
        ui.set_visible(self.chrome.icons.id(), !details);
        ui.set_visible(self.chrome.details.id(), details);
        // The switch shows the other view's icon.
        let icon = if details {
            Lucide::LayoutGrid
        } else {
            Lucide::List
        };
        self.chrome.view_switch.set_icon(Some(icon));
        self.show_sort_indicator();
        ui.focus(self.active_view());
    }

    /// Switches between icons and details, keeping the selection.
    pub(super) fn toggle_view(&mut self, ui: &mut Ui<Msg>) {
        let selection = self.selection();
        self.options.mode = match self.options.mode {
            ViewMode::Icons => ViewMode::Details,
            ViewMode::Details => ViewMode::Icons,
        };
        self.apply_mode(ui);
        self.set_selection(&selection);
        if let Some(first) = selection.first() {
            self.ensure_visible(*first);
        }
        self.publish_view();
    }

    /// The selection changed: remember the one before it.
    pub(super) fn selection_changed(&mut self) {
        let now = self.selection();
        if now != self.selected {
            self.previous = std::mem::replace(&mut self.selected, now);
        }
        self.publish_view();
        self.update_status();
    }

    /// Tells the shell what this window shows (for drag and drop and
    /// refreshes) and the session what it has selected.
    pub(super) fn publish_view(&self) {
        let paths = |rows: &[usize]| {
            self.listing
                .names_of(rows)
                .into_iter()
                .map(|name| self.dir.join(name))
                .collect::<Vec<PathBuf>>()
        };
        let selected = paths(&self.selected);
        self.explorer
            .session()
            .selection_changed(&self.dir, &selected);
        self.explorer.publish_view(ViewState {
            window: self.window,
            dir: self.dir.clone(),
            view: self.active_view(),
            selected,
            previous: paths(&self.previous),
            proxy: self.proxy.clone(),
        });
    }

    /// Rewrites the status bar from the listing and the selection, or shows
    /// the read error when the folder could not be listed.
    pub(super) fn update_status(&self) {
        match &self.listing.error {
            Some(error) => self.chrome.status.set_parts(&[error]),
            None => {
                let parts = summarize(&self.listing.entries, &self.selection());
                let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
                self.chrome.status.set_parts(&refs);
            }
        }
    }

    /// Shows the sort menu under the Sort button, checked to the order.
    pub(super) fn show_sort_menu(&mut self, ui: &mut Ui<Msg>) {
        for key in SortKey::ALL {
            self.sort_menu
                .set_checked(sort_id(key), key == self.options.sort.key);
        }
        self.sort_menu
            .set_checked(SORT_DESCENDING, self.options.sort.descending);
        let bounds = ui.bounds(self.chrome.sort.id());
        self.sort_menu.show_context(bounds.left, bounds.bottom);
    }

    /// A sort menu item: a key sorts by it, Descending flips the direction.
    pub(super) fn sort_chosen(&mut self, id: MenuId, ui: &Ui<Msg>) {
        let order = self.options.sort;
        let order = match sort_key_of(id) {
            Some(key) => SortOrder { key, ..order },
            None if id == SORT_DESCENDING => SortOrder {
                descending: !order.descending,
                ..order
            },
            None => return,
        };
        self.sort_by(order, ui);
    }

    /// A details header click: the same column flips, another starts
    /// ascending.
    pub(super) fn sort_column(&mut self, column: usize, ui: &Ui<Msg>) {
        if let Some(key) = SortKey::of_column(column) {
            self.sort_by(self.options.sort.toggled(key), ui);
        } else {
            self.show_sort_indicator();
        }
    }

    /// Re-sorts the listing in `order`, keeping the selection by name.
    fn sort_by(&mut self, order: SortOrder, ui: &Ui<Msg>) {
        let keep = self.listing.names_of(&self.selection());
        self.options.sort = order;
        self.listing = Rc::new(self.listing.sorted(order));
        self.set_models(ui);
        let rows = self.listing.indices_of(&keep);
        self.set_selection(&rows);
        if let Some(first) = rows.first() {
            self.ensure_visible(*first);
        }
        self.selected = rows;
        self.previous.clear();
        self.show_sort_indicator();
        self.publish_view();
        self.update_status();
    }

    /// Puts the details header's arrow on the sorted column (the list moves
    /// its own arrow on a click; the window's order is the truth).
    fn show_sort_indicator(&self) {
        let direction = if self.options.sort.descending {
            SortDirection::Descending
        } else {
            SortDirection::Ascending
        };
        self.chrome
            .details
            .set_sort_indicator(self.options.sort.key.column(), direction);
    }
}
