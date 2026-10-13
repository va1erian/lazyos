//! The Default apps page: which web browser opens links, for the user running Settings
//! (docs/lazywebplus.md).
//!
//! One list: Automatic, then each installed browser. A pick saves at once through
//! [`defaults_ops`]; `mimed` reads the choice each time something opens a web link, so it applies
//! to the next link, with no restart.

use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{column, label, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::layout::Align;
use xui_core::widget::{Label, ListView};
use xui_core::HasText;

use crate::app::{choice_list, Msg};
use crate::defaults_ops::{self, Choices};
use crate::store::ConfigStore;

/// Messages the Default apps page's widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum DefaultsMsg {
    /// Row `.0` of the browser list was picked (0: Automatic).
    Browser(usize),
}

/// The page's widgets and working state.
pub struct DefaultsPage {
    list: Rc<ListView<Msg>>,
    hint: Rc<Label<Msg>>,
    _mounted: Mounted<Msg>,
    /// `None` when the user is unknown: the page then only explains why.
    choices: Option<Choices>,
}

impl DefaultsPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<DefaultsPage> {
        let (list, hint) = (Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().padding(20).gap(8).children((
                label("Web browser"),
                choice_list(&[defaults_ops::AUTOMATIC])
                    .on_select(|row| Msg::Defaults(DefaultsMsg::Browser(row)))
                    .bind(&list)
                    .size(440, 110)
                    .align(Align::Start),
                label("Opens links from Mail, Files and the Terminal, and HTML files.").bind(&hint),
            )),
        )?;
        Ok(DefaultsPage {
            list: list.get(),
            hint: hint.get(),
            _mounted: mounted,
            choices: None,
        })
    }

    /// Re-read the registry and the user's choice, and repaint the list.
    pub fn load(&mut self, store: &dyn ConfigStore) {
        match defaults_ops::load(store) {
            Ok(choices) => {
                self.choices = Some(choices);
                self.hint
                    .set_text("Opens links from Mail, Files and the Terminal, and HTML files.");
            }
            Err(error) => {
                self.choices = None;
                self.hint.set_text(&format!("Unavailable: {error}."));
            }
        }
        self.refresh();
    }

    /// Rebuild the rows from the working state.
    fn refresh(&self) {
        let rows = self.choices.as_ref().map_or_else(
            || vec![String::from(defaults_ops::AUTOMATIC)],
            Choices::rows,
        );
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        self.list.set_items(&rows);
        self.list
            .select(Some(self.choices.as_ref().map_or(0, Choices::selected_row)));
    }

    /// Handle one message; returns the status line text.
    pub fn update(&mut self, msg: DefaultsMsg, store: &dyn ConfigStore) -> String {
        let Some(choices) = self.choices.as_mut() else {
            self.refresh();
            return String::from("Default apps are unavailable.");
        };
        let DefaultsMsg::Browser(row) = msg;
        match defaults_ops::choose(store, choices, row) {
            Ok(text) => {
                self.refresh();
                text
            }
            Err(error) => {
                // The row already looks picked: show what is stored.
                self.load(store);
                format!("Could not change the default browser: {error}")
            }
        }
    }
}
