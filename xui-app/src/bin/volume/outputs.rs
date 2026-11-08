//! The Volume applet's output list: what `ListOutputs` says, as menu rows
//! (one radio row per output under an "Output" heading), and the user's
//! choice kept in `confd` (`user/<uid>/audio/output`).

use audioclient::{control_wire as control, MixerControl, Transport};
use trayclient::{menu_row, wire};
use xui_app::platform::confd_store::ConfdStore;
use xui_app::trace::{self, Channel};
use xui_settings::store::{ConfigStore, Value};

/// Menu ids: the separator and heading, then one row per output from
/// [`FIRST_ROW`] in list order.
const ROW_SEPARATOR: u32 = 10;
const ROW_HEADING: u32 = 11;
const FIRST_ROW: u32 = 100;
/// Outputs shown at most (a menu, not a device manager).
const MAX_ROWS: usize = 16;

#[derive(Default)]
pub(super) struct Outputs {
    list: Vec<control::Output>,
}

impl Outputs {
    /// Read the list again; whether it changed.
    pub(super) fn refresh<T: Transport>(&mut self, mixer: &MixerControl<T>) -> bool {
        let Ok(mut list) = mixer.outputs() else {
            return false;
        };
        // The mixer lists the playing output first; the menu keeps its rows
        // where they were when the choice changes.
        list.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        list.truncate(MAX_ROWS);
        if list == self.list {
            return false;
        }
        let ids: Vec<&str> = list.iter().map(|o| o.id.as_str()).collect();
        trace::line!(Channel::Volume, "OUTPUTS {}", ids.join(","));
        self.list = list;
        true
    }

    /// The name of the output playing, for the tooltip.
    pub(super) fn active_name(&self) -> &str {
        self.list
            .iter()
            .find(|o| o.active)
            .map_or("no output", |o| o.name.as_str())
    }

    /// The menu rows: nothing for a single output (there is no choice).
    pub(super) fn rows(&self) -> Vec<wire::MenuItem> {
        if self.list.len() < 2 {
            return Vec::new();
        }
        let mut heading = menu_row(ROW_HEADING, "Output", wire::MENU_KIND_NORMAL);
        heading.enabled = false;
        let mut rows = vec![
            menu_row(ROW_SEPARATOR, "", wire::MENU_KIND_SEPARATOR),
            heading,
        ];
        for (index, output) in self.list.iter().enumerate() {
            let mut row = menu_row(FIRST_ROW + index as u32, &output.name, wire::MENU_KIND_RADIO);
            row.checked = output.active;
            rows.push(row);
        }
        rows
    }

    /// The output a menu row stands for.
    pub(super) fn row_output(&self, id: u32) -> Option<String> {
        let index = usize::try_from(id.checked_sub(FIRST_ROW)?).ok()?;
        self.list.get(index).map(|o| o.id.clone())
    }

    /// Ask the mixer for the remembered output again when it is present
    /// and not preferred (the mixer restarted, or it just came back).
    pub(super) fn reapply<T: Transport>(&mut self, mixer: &MixerControl<T>) {
        let Some(saved) = remembered() else {
            return;
        };
        let pending = self.list.iter().any(|o| o.id == saved && !o.preferred);
        if pending && mixer.select_output(&saved).is_ok() {
            trace::line!(Channel::Volume, "OUTPUT:{saved}");
            self.refresh(mixer);
        }
    }
}

fn key() -> Option<String> {
    let uid = ConfdStore::new().uid()?;
    Some(format!("user/{uid}/audio/output"))
}

/// The output this user picked last.
fn remembered() -> Option<String> {
    match ConfdStore::new().get(&key()?) {
        Some(Value::Str(id)) if !id.is_empty() => Some(id),
        _ => None,
    }
}

/// Keep `id` as this user's output (losing it only means picking again).
pub(super) fn remember(id: &str) {
    if let Some(key) = key() {
        let _ = ConfdStore::new().set(&key, Value::Str(id.to_owned()));
    }
}
