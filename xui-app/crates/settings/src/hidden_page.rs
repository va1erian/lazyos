//! The Hidden apps page: one checkbox per registry app; a checked app is left
//! out of the start menu for the user running Settings (issue #509 §5).
//!
//! Every toggle saves through [`hidden_ops`] at once. LazyShell rebuilds the
//! menu each time it opens, so the change shows on the next open. Hiding is a
//! menu matter only: a hidden app still launches and opens files.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{button, column, label, tree_view, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::Align;
use xui_core::widget::{CheckState, Label, TreeRow, TreeView};
use xui_core::HasText;

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

/// The page's widgets and working state.
pub struct HiddenPage {
    tree: Rc<TreeView<Msg>>,
    hint: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
    /// `None` when the user is unknown: the page then only explains why.
    list: Option<HiddenList>,
}

impl HiddenPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<HiddenPage> {
        let (tree, hint) = (Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().padding(20).gap(8).children((
                label("Hide from the start menu"),
                tree_view()
                    .then(|tree| {
                        tree.checkboxes(true)
                            .indent_guides(false)
                            .on_check(|row, state| {
                                Some(Msg::Hidden(HiddenMsg::Toggle(
                                    row,
                                    state == CheckState::Checked,
                                )))
                            })
                    })
                    .bind(&tree)
                    .size(440, 300)
                    .align(Align::Start),
                label("Hidden apps still run and open files.").bind(&hint),
                button("Reset to defaults")
                    .on_click(Msg::Hidden(HiddenMsg::Reset))
                    .align(Align::Start),
            )),
        )?;
        Ok(HiddenPage {
            tree: tree.get(),
            hint: hint.get(),
            _mounted: mounted,
            list: None,
        })
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
