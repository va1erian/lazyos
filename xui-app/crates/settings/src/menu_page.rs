//! The Menu page: the apps pinned to the start menu's root (`sys/ui/menu`),
//! above the power rows. Every app is in its category's submenu anyway, so
//! nothing is pinned by default.
//!
//! Left, the menu's entries; right, the registry apps not in it yet. The menu
//! is a machine setting whose every write asks an administrator, so the
//! buttons edit a draft and **Save** writes it in one request ([`menu_ops`]);
//! **Revert** goes back to what is stored. A refused save keeps the draft,
//! so nothing typed is lost.

use std::rc::Rc;

use deskmenu::Entry;
use xui_core::app::Ui;
use xui_core::arrange::{button, column, edit, label, row, Build, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{Button, Edit, Label, ListView};
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
    /// Write the draft (one request).
    Save,
    /// Drop the draft and show the stored menu again.
    Revert,
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
    state: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
    /// The draft the buttons edit.
    list: Vec<Entry>,
    /// What the store holds.
    saved: Vec<Entry>,
    apps: Vec<AppChoice>,
    free: Vec<AppChoice>,
}

impl MenuPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<MenuPage> {
        let (entries, available, rename) = (Handle::new(), Handle::new(), Handle::new());
        let state = Handle::new();
        // Both columns end in rows of buttons, so the lists line up.
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
                        row().gap(6).children((
                            command("Save", MenuMsg::Save),
                            command("Revert", MenuMsg::Revert),
                            label("").bind(&state).fill(1),
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
            state: state.get(),
            _mounted: mounted,
            list: Vec::new(),
            saved: Vec::new(),
            apps: Vec::new(),
            free: Vec::new(),
        })
    }

    /// Re-read the store and the registry and repaint both lists.
    pub fn load(&mut self, store: &dyn ConfigStore) {
        self.apps = store.apps();
        self.saved = menu_ops::load(store);
        self.list = self.saved.clone();
        self.refresh(Some(0));
    }

    /// Whether the draft differs from the stored menu.
    pub fn unsaved(&self) -> bool {
        self.list != self.saved
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
        self.state.set_text(if self.unsaved() {
            "Unsaved changes"
        } else {
            ""
        });
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
        let list = &mut self.list;
        let result = match msg {
            // The row the message names (a session or a test may raise it).
            MenuMsg::Select(row) => {
                self.entries
                    .select(Some(row).filter(|row| *row < list.len()));
                self.fill_rename();
                return String::new();
            }
            MenuMsg::Pick(row) => {
                let free = self.free.len();
                self.available.select(Some(row).filter(|row| *row < free));
                return String::new();
            }
            MenuMsg::Save => return self.save(store),
            MenuMsg::Revert => {
                let at = selected.unwrap_or(0);
                self.list = self.saved.clone();
                self.refresh(Some(at));
                return String::from("Changes dropped.");
            }
            MenuMsg::Up | MenuMsg::Down => {
                let Some(i) = selected else {
                    return needs_entry();
                };
                let delta = if msg == MenuMsg::Up { -1 } else { 1 };
                menu_ops::move_by(list, i, delta).map(Some)
            }
            MenuMsg::Remove => {
                let Some(i) = selected else {
                    return needs_entry();
                };
                menu_ops::remove(list, i).map(Some)
            }
            MenuMsg::Rename => {
                let Some(i) = selected else {
                    return needs_entry();
                };
                let label = self.rename.text();
                menu_ops::rename(list, i, &label).map(|()| Some(i))
            }
            MenuMsg::Add => match self.available.selected().and_then(|i| self.free.get(i)) {
                Some(app) => menu_ops::add(list, &app.clone()).map(Some),
                None => return String::from("Select an app to add first."),
            },
            MenuMsg::Reset => {
                menu_ops::reset(list);
                Ok(Some(0))
            }
        };
        match result {
            Ok(select) => {
                self.refresh(select);
                String::from("Changed; press Save to apply it.")
            }
            Err(error) => format!("Could not change the menu: {error}"),
        }
    }

    /// Write the draft in one request.
    fn save(&mut self, store: &dyn ConfigStore) -> String {
        if !self.unsaved() {
            return String::from("Nothing to save.");
        }
        match menu_ops::save(store, &self.list) {
            Ok(()) => {
                println!("SETTINGS:MENU:SAVED:{}", self.list.len());
                self.saved = self.list.clone();
                let at = self.entries.selected();
                self.refresh(at);
                String::from("Menu saved.")
            }
            Err(error) => format!("Menu not saved: {error}"),
        }
    }
}
