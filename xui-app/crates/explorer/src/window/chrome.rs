#![forbid(unsafe_code)]

//! The window's layout and menus: the browser-style toolbar (Back, Forward,
//! Up, the address bar, Sort, folder Properties and the view switch), the
//! icon and details views stacked in one place, the status bar, the item
//! context menu and the sort menu.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{
    Align, Build, Handle, Layout, LayoutExt, button, column, edit, icon_view_with, list, row,
    stack, status_bar,
};
use xui_core::icon::Lucide;
use xui_core::layout::Insets;
use xui_core::units::Dip;
use xui_core::widget::{Button, Edit, Fill, IconView, ListView, Menu, MenuId, StatusBar};

use super::Msg;
use crate::model::{SharedListing, SortKey};

/// The toolbar's height and its square buttons' side.
const TOOLBAR_HEIGHT: Dip = Dip(38.0);
const TOOL_SIDE: Dip = Dip(30.0);

/// The details view's fixed columns (Name takes the rest).
const SIZE_WIDTH: Dip = Dip(90.0);
const TYPE_WIDTH: Dip = Dip(110.0);
const MODIFIED_WIDTH: Dip = Dip(140.0);

/// The item context menu's command ids.
pub(super) const MENU_OPEN: MenuId = MenuId::new(0);
pub(super) const MENU_DELETE: MenuId = MenuId::new(1);
pub(super) const MENU_PROPERTIES: MenuId = MenuId::new(2);
pub(super) const MENU_REFRESH: MenuId = MenuId::new(3);
pub(super) const MENU_COPY: MenuId = MenuId::new(4);
pub(super) const MENU_PASTE: MenuId = MenuId::new(5);
pub(super) const MENU_OPEN_WINDOW: MenuId = MenuId::new(6);

/// The sort menu: one radio item per key from this id up, then the
/// direction.
const SORT_KEY_BASE: usize = 10;
/// The sort menu's Descending check.
pub const SORT_DESCENDING: MenuId = MenuId::new(20);

/// The sort menu's id for `key`.
pub fn sort_id(key: SortKey) -> MenuId {
    MenuId::new(SORT_KEY_BASE + key.column())
}

/// The key a sort menu id picks.
pub(super) fn sort_key_of(id: MenuId) -> Option<SortKey> {
    SortKey::ALL.into_iter().find(|key| sort_id(*key) == id)
}

/// The handles the layout binds, read back once it is mounted.
#[derive(Default)]
pub(super) struct Handles {
    back: Handle<Button<Msg>>,
    forward: Handle<Button<Msg>>,
    up: Handle<Button<Msg>>,
    address: Handle<Edit<Msg>>,
    sort: Handle<Button<Msg>>,
    view_switch: Handle<Button<Msg>>,
    icons: Handle<IconView<Msg>>,
    details: Handle<ListView<Msg>>,
    status: Handle<StatusBar<Msg>>,
}

/// The mounted widgets.
pub(super) struct Chrome {
    pub(super) back: Rc<Button<Msg>>,
    pub(super) forward: Rc<Button<Msg>>,
    pub(super) up: Rc<Button<Msg>>,
    pub(super) address: Rc<Edit<Msg>>,
    pub(super) sort: Rc<Button<Msg>>,
    pub(super) view_switch: Rc<Button<Msg>>,
    pub(super) icons: Rc<IconView<Msg>>,
    pub(super) details: Rc<ListView<Msg>>,
    pub(super) status: Rc<StatusBar<Msg>>,
}

impl Handles {
    /// The window: the toolbar, the two views in one place, the status bar.
    pub(super) fn layout(&self, model: SharedListing) -> Layout<Msg> {
        column().children((
            self.toolbar().fixed(TOOLBAR_HEIGHT),
            stack()
                .children((
                    icon_view_with(model)
                        .on_activate(Msg::Activate)
                        .then(|view| {
                            view.multi_select(true)
                                .on_selection(|_| Some(Msg::Selection))
                                .on_context(|item, at| Some(Msg::Context(item, at)))
                        })
                        .bind(&self.icons),
                    details_list().bind(&self.details),
                ))
                .fill(1),
            status_bar(&[""]).bind(&self.status),
        ))
    }

    /// Back, Forward and Up; the address bar; Sort, Properties and the view
    /// switch.
    fn toolbar(&self) -> Layout<Msg> {
        row()
            .gap(6)
            .align(Align::Center)
            .padding(Insets::symmetric(Dip(6.0), Dip(4.0)))
            .children((
                row().gap(2).align(Align::Center).children((
                    tool(Lucide::ChevronLeft, "Back (Alt+Left)", Msg::Back)
                        .bind(&self.back)
                        .width(TOOL_SIDE),
                    tool(Lucide::ChevronRight, "Forward (Alt+Right)", Msg::Forward)
                        .bind(&self.forward)
                        .width(TOOL_SIDE),
                    tool(Lucide::ChevronUp, "Up one level (Alt+Up)", Msg::Up)
                        .bind(&self.up)
                        .width(TOOL_SIDE),
                )),
                edit()
                    .placeholder("Type a folder and press Enter")
                    .on_change(|_| Msg::AddressEdited)
                    .bind(&self.address)
                    .fill(1),
                row().gap(2).align(Align::Center).children((
                    tool(Lucide::ArrowDownAZ, "Sort by", Msg::SortMenu)
                        .bind(&self.sort)
                        .width(TOOL_SIDE),
                    tool(Lucide::Info, "Folder properties", Msg::FolderProperties).width(TOOL_SIDE),
                    tool(
                        Lucide::List,
                        "Switch view: icons or details",
                        Msg::ToggleView,
                    )
                    .bind(&self.view_switch)
                    .width(TOOL_SIDE),
                )),
            ))
    }

    /// The mounted widgets. Call once [`layout`](Self::layout) is mounted.
    pub(super) fn get(&self) -> Chrome {
        Chrome {
            back: self.back.get(),
            forward: self.forward.get(),
            up: self.up.get(),
            address: self.address.get(),
            sort: self.sort.get(),
            view_switch: self.view_switch.get(),
            icons: self.icons.get(),
            details: self.details.get(),
            status: self.status.get(),
        }
    }
}

/// A square icon button with a tooltip.
fn tool(icon: Lucide, tip: &str, msg: Msg) -> Build<Button<Msg>, Msg> {
    button("").icon(icon).tooltip(tip).on_click(msg)
}

/// The details view: one row per entry under sortable Name, Size, Type and
/// Modified headers.
fn details_list() -> Build<ListView<Msg>, Msg> {
    list()
        .column(SortKey::Name.label(), Fill)
        .column_right(SortKey::Size.label(), SIZE_WIDTH)
        .column(SortKey::Type.label(), TYPE_WIDTH)
        .column(SortKey::Modified.label(), MODIFIED_WIDTH)
        .on_activate(Msg::Activate)
        .then(|list| {
            list.multi_select(true)
                .on_selection(|_| Some(Msg::Selection))
                .on_context(|row, at| Some(Msg::Context(Some(row), at)))
                .on_sort(|column| Some(Msg::SortColumn(column)))
        })
}

/// The item context menu. Its items are enabled per right click.
pub(super) fn context_menu(ui: &Ui<Msg>) -> Menu<Msg> {
    Menu::context(ui)
        .build(|scope| {
            scope.item(MENU_OPEN, "Open");
            scope.item(MENU_OPEN_WINDOW, "Open in New Window");
            scope.separator();
            scope.item(MENU_COPY, "Copy");
            scope.item(MENU_PASTE, "Paste");
            scope.item(MENU_DELETE, "Delete");
            scope.separator();
            scope.item(MENU_PROPERTIES, "Properties");
            scope.item(MENU_REFRESH, "Refresh");
        })
        .on_select(|id| Some(Msg::Menu(id)))
}

/// The sort menu under the Sort button: the keys as radio items, then the
/// direction. Its checks are set from the window's order before it shows.
pub(super) fn sort_menu(ui: &Ui<Msg>) -> Menu<Msg> {
    Menu::context(ui)
        .build(|scope| {
            for key in SortKey::ALL {
                scope.radio(sort_id(key), key.label(), key == SortKey::Name);
            }
            scope.separator();
            scope.check(SORT_DESCENDING, "Descending", false);
        })
        .on_select(|id| Some(Msg::SortChosen(id)))
}
