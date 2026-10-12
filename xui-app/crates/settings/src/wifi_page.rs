//! The Wi-Fi page: the regulatory country the radio follows
//! ([`wifi_ops`]).
//!
//! The country is a machine setting (`sys/net/wifi/country`), so choosing
//! one only selects it; **Use this country** writes it, once, through the
//! store's elevation (an administrator approves on the trusted prompt), as
//! the keyboard layout's **Make it the default** does. A search field
//! narrows the list of countries by name or code. The country of the
//! configured time zone is offered as a suggestion when the table knows one.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::Ui;
use xui_core::arrange::{button, column, edit, label, row, Handle, LayoutExt, Mounted};
use xui_core::backend::{Result, WidgetId};
use xui_core::widget::{Edit, Label, ListView};
use xui_core::HasText;

use crate::app::{choice_list, Msg};
use crate::store::ConfigStore;
use crate::system::System;
use crate::wifi_ops::{self, Entry};

/// The widest the search field and the list get.
const FIELD_W: i32 = 340;

/// Messages the Wi-Fi page's widgets raise.
#[derive(Clone, Debug, PartialEq)]
pub enum WifiMsg {
    /// The search text changed.
    Filter(String),
    /// A row of the (filtered) list was selected; nothing is written.
    Select(usize),
    /// Write the selected country (asks an administrator).
    Apply,
    /// Write the suggested country (asks an administrator).
    ApplySuggestion,
}

/// The page's widgets.
pub struct WifiPage {
    current: Rc<Label<Msg>>,
    suggestion: Rc<Label<Msg>>,
    search: Rc<Edit<Msg>>,
    list: Rc<ListView<Msg>>,
    entries: Vec<Entry>,
    /// The entries the list shows now, as indexes into `entries`.
    visible: RefCell<Vec<usize>>,
    /// The country the time zone suggests, when it differs from the stored one.
    suggested: RefCell<Option<&'static str>>,
    _mounted: Mounted<Msg>,
}

impl WifiPage {
    /// Lays the page out in the container `page`.
    pub fn build(ui: &Ui<Msg>, page: WidgetId) -> Result<WifiPage> {
        let (current, suggestion, search, list) =
            (Handle::new(), Handle::new(), Handle::new(), Handle::new());
        let mounted = ui.mount_in(
            page,
            column().padding(20).gap(8).children((
                label("Regulatory country"),
                label("").bind(&current),
                label("").bind(&suggestion),
                row().child(
                    button("Use the suggestion")
                        .on_click(Msg::Wifi(WifiMsg::ApplySuggestion)),
                ),
                edit()
                    .placeholder("Search the countries")
                    .on_change(|text| Msg::Wifi(WifiMsg::Filter(text)))
                    .bind(&search)
                    .max_width(FIELD_W),
                choice_list(&[])
                    .on_select(|i| Msg::Wifi(WifiMsg::Select(i)))
                    .bind(&list)
                    .max_width(FIELD_W)
                    .fill(1),
                row().child(button("Use this country").on_click(Msg::Wifi(WifiMsg::Apply))),
                label("It limits the channels and power the Wi-Fi radio may use."),
                label("Changing it asks an administrator."),
            )),
        )?;
        Ok(WifiPage {
            current: current.get(),
            suggestion: suggestion.get(),
            search: search.get(),
            list: list.get(),
            entries: wifi_ops::entries(),
            visible: RefCell::new(Vec::new()),
            suggested: RefCell::new(None),
            _mounted: mounted,
        })
    }

    /// Show the stored country, offer the time zone's, and list every
    /// country with the stored one selected (no event is raised).
    pub fn load(&self, store: &dyn ConfigStore, system: &dyn System) {
        let stored = wifi_ops::current(store);
        self.current.set_text(&wifi_ops::describe_current(store));
        let zone = system.now().map(|now| now.zone);
        let suggested = zone
            .as_deref()
            .and_then(|zone| wifi_ops::suggestion(zone).map(|code| (zone, code)))
            .filter(|(_, code)| stored.as_deref() != Some(*code));
        self.suggestion.set_text(&match (&zone, suggested) {
            (_, Some((zone, code))) => format!(
                "Suggested for your time zone ({zone}): {}.",
                wifi_ops::label_of(code).unwrap_or_default()
            ),
            (Some(zone), None) if wifi_ops::suggestion(zone).is_some() => {
                format!("That is the country of your time zone ({zone}).")
            }
            (Some(zone), None) => format!("No suggestion for the time zone {zone}."),
            (None, None) => String::from("The time zone is unknown: no suggestion."),
        });
        *self.suggested.borrow_mut() = suggested.map(|(_, code)| code);
        self.search.set_text("");
        self.show(&wifi_ops::filter(&self.entries, ""));
        let at = stored
            .as_deref()
            .or(Some(iso3166::WORLD))
            .and_then(|code| self.visible.borrow().iter().position(|i| self.entries[*i].code == code));
        self.list.select(at);
        if let Some(at) = at {
            self.list.ensure_visible(at);
        }
    }

    /// Fill the list with `rows` (indexes into the entries).
    fn show(&self, rows: &[usize]) {
        let labels: Vec<&str> = rows
            .iter()
            .map(|row| self.entries[*row].label.as_str())
            .collect();
        self.list.set_items(&labels);
        *self.visible.borrow_mut() = rows.to_vec();
    }

    /// The code of the row selected in the list, not yet written.
    pub fn selected(&self) -> Option<&'static str> {
        let row = self.list.selected()?;
        let entry = *self.visible.borrow().get(row)?;
        Some(self.entries[entry].code)
    }

    /// The country the time zone suggests, if it differs from the stored one.
    pub fn suggested(&self) -> Option<&'static str> {
        *self.suggested.borrow()
    }

    /// Handle a message that writes nothing; returns the status line text.
    /// `Apply` and `ApplySuggestion` are the window's (they wait on an
    /// administrator, so they run on a worker).
    pub fn update(&self, msg: WifiMsg) -> String {
        match msg {
            WifiMsg::Filter(text) => {
                let rows = wifi_ops::filter(&self.entries, &text);
                self.show(&rows);
                // The first match is the one a plain Enter-less flow picks.
                self.list.select(if rows.is_empty() { None } else { Some(0) });
                match rows.len() {
                    0 => String::from("No country matches."),
                    n => format!("{n} countries match."),
                }
            }
            WifiMsg::Select(row) => {
                let shown = self.visible.borrow().len();
                self.list.select(Some(row).filter(|row| *row < shown));
                String::new()
            }
            WifiMsg::Apply | WifiMsg::ApplySuggestion => String::new(),
        }
    }
}
