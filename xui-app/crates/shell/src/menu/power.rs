//! The start menu's power rows (docs/shutdown.md): "Restart..." and
//! "Shut down..." at the bottom, after the configured entries. Choosing one
//! swaps the two rows for a confirmation in place ("Restart now" /
//! "Shut down now", then "Cancel"), so the panel keeps its size; only the
//! confirmation row asks `init` to stop the machine.

use super::{Menu, Row};

/// What the machine does once `init` has stopped everything (`init`'s
/// `PowerMode`; the shell maps it to the generated constant).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    PowerOff,
    Reboot,
}

impl Power {
    /// The first step's label.
    fn ask_label(self) -> &'static str {
        match self {
            Power::Reboot => "Restart...",
            Power::PowerOff => "Shut down...",
        }
    }

    /// The confirmation's label.
    fn confirm_label(self) -> &'static str {
        match self {
            Power::Reboot => "Restart now",
            Power::PowerOff => "Shut down now",
        }
    }
}

/// What a row does when chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Launch the registry app [`Row::app`].
    Launch,
    /// A power row: show the confirmation.
    Ask(Power),
    /// The confirmation: ask `init` to stop the machine.
    Confirm(Power),
    /// Leave the confirmation.
    Cancel,
    /// A category row: opens the submenu of category `n`
    /// ([`Menu::submenu`]).
    Submenu(usize),
}

/// The outcome of choosing a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Nothing (a disabled row, or none).
    Nothing,
    /// Close the menu and launch this app.
    Launch(String),
    /// The confirmation is now showing; the menu stays open.
    Confirming(Power),
    /// Close the menu and ask `init` for this stop.
    Request(Power),
    /// Close the menu (the confirmation was cancelled).
    Close,
    /// Open the submenu of the category row chosen.
    Submenu(usize),
}

/// How many rows the power section always takes.
pub const POWER_ROWS: usize = 2;

fn row(label: &str, action: Action) -> Row {
    Row {
        app: String::new(),
        label: String::from(label),
        enabled: true,
        action,
    }
}

/// The two rows in their first step.
pub(super) fn ask_rows() -> [Row; POWER_ROWS] {
    [Power::Reboot, Power::PowerOff].map(|power| row(power.ask_label(), Action::Ask(power)))
}

/// The two rows while `power` waits for confirmation.
fn confirm_rows(power: Power) -> [Row; POWER_ROWS] {
    [
        row(power.confirm_label(), Action::Confirm(power)),
        row("Cancel", Action::Cancel),
    ]
}

impl Menu {
    /// Choose row `index`. `repeat` is the second press of a double click:
    /// it never confirms, so a double click on "Restart..." does not stop the
    /// machine through the row that appeared under the pointer.
    pub fn choose(&mut self, index: usize, repeat: bool) -> Choice {
        let Some(row) = self.rows.get(index).filter(|row| row.enabled) else {
            return Choice::Nothing;
        };
        match row.action {
            Action::Launch => Choice::Launch(row.app.clone()),
            Action::Ask(power) => {
                self.set_power_rows(confirm_rows(power));
                Choice::Confirming(power)
            }
            Action::Confirm(_) if repeat => Choice::Nothing,
            Action::Confirm(power) => Choice::Request(power),
            Action::Cancel => {
                self.set_power_rows(ask_rows());
                Choice::Close
            }
            Action::Submenu(_) => Choice::Submenu(index),
        }
    }

    /// Whether a confirmation is showing.
    pub fn confirming(&self) -> bool {
        self.rows
            .iter()
            .any(|row| matches!(row.action, Action::Confirm(_)))
    }

    /// Replace the last [`POWER_ROWS`] rows (always the power section).
    fn set_power_rows(&mut self, rows: [Row; POWER_ROWS]) {
        let start = self.rows.len().saturating_sub(POWER_ROWS);
        self.rows.truncate(start);
        self.rows.extend(rows);
    }
}
