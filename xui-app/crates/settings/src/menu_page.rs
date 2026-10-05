//! The Menu page: the apps pinned to the start menu's root (`sys/ui/menu`),
//! above the power rows. Every app is in its category's submenu anyway, so
//! nothing is pinned by default.
//!
//! Left, the menu's entries; right, the registry apps not in it yet. Every
//! button saves through [`menu_ops`] at once, where `xuid` follows the key
//! live. The page owns the working list and only ever shows what was saved.

use std::rc::Rc;

use deskmenu::Entry;
use xui_core::app::Ui;
use xui_core::arrange::{button, column, edit, label, row, Build, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{Button, Edit, ListView};
use xui_core::HasText;

use crate::app::{choice_list, Msg};
use crate::menu_ops;
use crate::store::{AppChoice, ConfigStore};

/// Messages the Menu page's widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuMsg {
    /// An entry row was selected.
    Select(usize),
    /// An available-app row was selected.
    Pick(usize),
    Up,
    Down,
    Remove,
    Rename,
    Add,
    Reset,
}

/// A button raising `msg`.
fn command(text: &str, msg: MenuMsg) -> Build<Button<Msg>, Msg> {
    button(text).on_click(Msg::Menu(msg))
}

/// An empty single-column list raising `msg` with the selected row.
fn rows(msg: fn(usize) -> MenuMsg) -> Build<ListView<Msg>, Msg> {
    choice_list(&[]).on_select(move |i| Msg::Menu(msg(i)))
}

/// The page's widgets and working state.
pub struct MenuPage {
    entries: Rc<ListView<Msg>>,
    available: Rc<ListView<Msg>>,
    rename: Rc<Edit<Msg>>,
    _mounted: Mounted<Msg>,
    list: Vec<Entry>,
    apps: Vec<AppChoice>,
    free: Vec<AppChoice>,
}

impl MenuPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<MenuPage> {
        let (entries, available, rename) = (Handle::new(), Handle::new(), Handle::new());
        // Both columns end in two rows of buttons, so the lists line up.
        let mounted = ui.mount_in(
            page,
            row().padding(20).gap(20).children((
                column()
                    .gap(8)
                    .children((
                        label("Pinned to the start menu"),
                        rows(MenuMsg::Select).bind(&entries).fill(1),
                        row().gap(6).children((
                            command("Move up", MenuMsg::Up),
                            command("Move down", MenuMsg::Down),
                            command("Unpin", MenuMsg::Remove),
                        )),
                        row().gap(6).children((
                            edit().placeholder("New label").bind(&rename).fill(1),
                            command("Rename", MenuMsg::Rename),
                        )),
                    ))
                    .fill(3),
                column()
                    .gap(8)
                    .children((
                        label("Apps you can pin"),
                        rows(MenuMsg::Pick).bind(&available).fill(1),
                        row().child(command("Pin", MenuMsg::Add)),
                        row().child(command("Reset to defaults", MenuMsg::Reset)),
                    ))
                    .fill(2),
            )),
        )?;
        Ok(MenuPage {
            entries: entries.get(),
            available: available.get(),
            rename: rename.get(),
            _mounted: mounted,
            list: Vec::new(),
            apps: Vec::new(),
            free: Vec::new(),
        })
    }

    /// Re-read the store and the registry and repaint both lists.
    pub fn load(&mut self, store: &dyn ConfigStore) {
        self.apps = store.apps();
        self.list = menu_ops::load(store);
        self.refresh(Some(0));
    }

    /// Rebuild both lists from the working state, selecting entry `select`.
    fn refresh(&mut self, select: Option<usize>) {
        let rows: Vec<String> = self.list.iter().map(menu_ops::row_text).collect();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        self.entries.set_items(&rows);
        self.entries.select(select.filter(|i| *i < rows.len()));
        self.free = menu_ops::available(&self.list, &self.apps);
        let names: Vec<&str> = self.free.iter().map(|a| a.name.as_str()).collect();
        self.available.set_items(&names);
        self.available
            .select(if names.is_empty() { None } else { Some(0) });
        self.fill_rename();
    }

    /// Show the selected entry's label in the rename box.
    fn fill_rename(&self) {
        let label = self
            .entries
            .selected()
            .and_then(|i| self.list.get(i))
            .map_or("", |e| e.label.as_str());
        self.rename.set_text(label);
    }

    /// Handle one message; returns the status line text.
    pub fn update(&mut self, msg: MenuMsg, store: &dyn ConfigStore) -> String {
        let selected = self.entries.selected();
        let needs_entry = || String::from("Select a menu entry first.");
        let result = match msg {
            MenuMsg::Select(_) => {
                self.fill_rename();
                return String::new();
            }
            MenuMsg::Pick(_) => return String::new(),
            MenuMsg::Up | MenuMsg::Down => {
                let Some(i) = selected else {
                    return needs_entry();
                };
                let delta = if msg == MenuMsg::Up { -1 } else { 1 };
                menu_ops::move_by(store, &mut self.list, i, delta).map(|to| (Some(to), "Moved."))
            }
            MenuMsg::Remove => {
                let Some(i) = selected else {
                    return needs_entry();
                };
                menu_ops::remove(store, &mut self.list, i).map(|to| (Some(to), "Removed."))
            }
            MenuMsg::Rename => {
                let Some(i) = selected else {
                    return needs_entry();
                };
                let label = self.rename.text();
                menu_ops::rename(store, &mut self.list, i, &label).map(|()| (Some(i), "Renamed."))
            }
            MenuMsg::Add => match self.available.selected().and_then(|i| self.free.get(i)) {
                Some(app) => {
                    let app = app.clone();
                    menu_ops::add(store, &mut self.list, &app).map(|to| (Some(to), "Added."))
                }
                None => return String::from("Select an app to add first."),
            },
            MenuMsg::Reset => menu_ops::reset(store, &mut self.list)
                .map(|()| (Some(0), "Menu reset to defaults.")),
        };
        match result {
            Ok((select, ok)) => {
                self.refresh(select);
                ok.to_owned()
            }
            Err(error) => format!("Could not change the menu: {error}"),
        }
    }
}
