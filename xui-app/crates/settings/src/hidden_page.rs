//! The Hidden apps page: one checkbox per registry app; a checked app is left
//! out of the start menu for the user running Settings (issue #509 §5).
//!
//! Every toggle saves through [`hidden_ops`] at once. LazyShell rebuilds the
//! menu each time it opens, so the change shows on the next open. Hiding is a
//! menu matter only: a hidden app still launches and opens files.

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{Button, CheckState, Label, Panel, TreeRow, TreeView};
use xui_core::{HasText, Rect};

use crate::app::Msg;
use crate::hidden_ops::{self, HiddenList};
use crate::store::ConfigStore;

/// Messages the Hidden apps page's widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum HiddenMsg {
    /// Row `.0`'s checkbox now reads `.1` (checked = hidden).
    Toggle(usize, bool),
    Reset,
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    crate::layout::rect(x, y, w, h)
}

/// The page's widgets and working state.
pub struct HiddenPage {
    panel: Panel<Msg>,
    tree: TreeView<Msg>,
    hint: Label<Msg>,
    _labels: Vec<Label<Msg>>,
    _buttons: Vec<Button<Msg>>,
    /// `None` when the user is unknown: the page then only explains why.
    list: Option<HiddenList>,
}

impl HiddenPage {
    /// Build the page (hidden state is the caller's job) inside `bounds`.
    pub fn build(ui: &Ui<Msg>, bounds: Rect) -> Result<HiddenPage> {
        let panel = Panel::new(ui, bounds)?;
        let (tree, hint, labels, buttons) = {
            let p = panel.ui();
            let labels = vec![Label::new(
                p,
                rect(20, 14, 450, 20),
                "Hide from the start menu",
            )?];
            let tree = TreeView::new(p, rect(20, 38, 450, 300), &[])?
                .checkboxes(true)
                .indent_guides(false)
                .on_check(|row, state| {
                    Some(Msg::Hidden(HiddenMsg::Toggle(
                        row,
                        state == CheckState::Checked,
                    )))
                });
            let hint = Label::new(
                p,
                rect(20, 346, 450, 20),
                "Hidden apps still run and open files.",
            )?;
            let buttons = vec![Button::new(p, rect(20, 374, 170, 30), "Reset to defaults")?
                .on_click(|| Some(Msg::Hidden(HiddenMsg::Reset)))];
            (tree, hint, labels, buttons)
        };
        Ok(HiddenPage {
            panel,
            tree,
            hint,
            _labels: labels,
            _buttons: buttons,
            list: None,
        })
    }

    pub fn set_visible(&self, visible: bool) {
        self.panel.set_visible(visible);
    }

    /// Re-read the registry and both layers of keys, and repaint the list.
    pub fn load(&mut self, store: &dyn ConfigStore) {
        match hidden_ops::load(store) {
            Ok(list) => {
                self.list = Some(list);
                self.hint.set_text("Hidden apps still run and open files.");
            }
            Err(error) => {
                self.list = None;
                self.hint.set_text(&format!("Unavailable: {error}."));
            }
        }
        self.refresh();
    }

    /// Rebuild the rows from the working state.
    fn refresh(&self) {
        let rows: Vec<TreeRow> = self
            .list
            .iter()
            .flat_map(|list| list.rows.iter())
            .map(|row| {
                let state = if row.hidden() {
                    CheckState::Checked
                } else {
                    CheckState::Unchecked
                };
                TreeRow::new(row.text(), 0).checked(state)
            })
            .collect();
        self.tree.set_rows(&rows);
    }

    /// Handle one message; returns the status line text.
    pub fn update(&mut self, msg: HiddenMsg, store: &dyn ConfigStore) -> String {
        let Some(list) = self.list.as_mut() else {
            self.refresh();
            return String::from("Hidden apps are unavailable.");
        };
        let result = match msg {
            HiddenMsg::Toggle(index, hide) => {
                let name = list.rows.get(index).map(|row| row.app.name.clone());
                hidden_ops::set(store, list, index, hide).map(|()| match (name, hide) {
                    (Some(name), true) => format!("{name} is hidden from the menu."),
                    (Some(name), false) => format!("{name} is shown in the menu."),
                    (None, _) => String::new(),
                })
            }
            HiddenMsg::Reset => hidden_ops::reset(store, list)
                .map(|_| String::from("Hidden apps reset to defaults.")),
        };
        match result {
            Ok(text) => {
                self.refresh();
                text
            }
            Err(error) => {
                // Re-read: the checkbox already flipped, and a failed reset
                // may have deleted some keys.
                self.load(store);
                format!("Could not change hidden apps: {error}")
            }
        }
    }
}
