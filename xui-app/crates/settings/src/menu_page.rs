//! The Menu page: edit the desktop right-click menu (`sys/ui/menu`).
//!
//! Left, the menu's entries; right, the registry apps not in it yet. Every
//! button saves through [`menu_ops`] at once, where `xuid` follows the key
//! live. The page owns the working list and only ever shows what was saved.

use deskmenu::Entry;
use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Button, Edit, Label, ListView, Panel};
use xui_core::{HasText, Rect};

use crate::app::Msg;
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

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    crate::layout::rect(x, y, w, h)
}

fn button(ui: &Ui<Msg>, bounds: Rect, text: &str, msg: MenuMsg) -> Result<Button<Msg>> {
    Ok(Button::new(ui, bounds, text)?.on_click(move || Some(Msg::Menu(msg.clone()))))
}

/// The page's widgets and working state.
pub struct MenuPage {
    panel: Panel<Msg>,
    entries: ListView<Msg>,
    available: ListView<Msg>,
    rename: Edit<Msg>,
    _labels: Vec<Label<Msg>>,
    _buttons: Vec<Button<Msg>>,
    list: Vec<Entry>,
    apps: Vec<AppChoice>,
    free: Vec<AppChoice>,
}

impl MenuPage {
    /// Build the page (hidden state is the caller's job) inside `bounds`.
    pub fn build(ui: &Ui<Msg>, bounds: Rect) -> Result<MenuPage> {
        let panel = Panel::new(ui, bounds)?;
        let (entries, available, rename, labels, buttons) = {
            let p = panel.ui();
            let labels = vec![
                Label::new(p, rect(20, 14, 220, 20), "Menu entries")?,
                Label::new(p, rect(260, 14, 210, 20), "Apps you can add")?,
            ];
            let entries = ListView::new(p, rect(20, 38, 220, 246), &[])?
                .multi_select(false)
                .on_select(|i| Some(Msg::Menu(MenuMsg::Select(i))));
            let available = ListView::new(p, rect(260, 38, 210, 246), &[])?
                .multi_select(false)
                .on_select(|i| Some(Msg::Menu(MenuMsg::Pick(i))));
            let rename = Edit::new(p, rect(20, 326, 150, 26), "")?.cue("New label");
            let buttons = vec![
                button(p, rect(20, 292, 70, 28), "Move up", MenuMsg::Up)?,
                button(p, rect(94, 292, 80, 28), "Move down", MenuMsg::Down)?,
                button(p, rect(178, 292, 62, 28), "Remove", MenuMsg::Remove)?,
                button(p, rect(176, 325, 64, 28), "Rename", MenuMsg::Rename)?,
                button(p, rect(260, 292, 70, 28), "Add", MenuMsg::Add)?,
                button(
                    p,
                    rect(260, 325, 140, 28),
                    "Reset to defaults",
                    MenuMsg::Reset,
                )?,
            ];
            (entries, available, rename, labels, buttons)
        };
        Ok(MenuPage {
            panel,
            entries,
            available,
            rename,
            _labels: labels,
            _buttons: buttons,
            list: Vec::new(),
            apps: Vec::new(),
            free: Vec::new(),
        })
    }

    pub fn set_visible(&self, visible: bool) {
        self.panel.set_visible(visible);
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
