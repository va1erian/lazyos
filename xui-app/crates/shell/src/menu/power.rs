//! The start menu's session rows (docs/shutdown.md, issue #623): "Log out...",
//! "Restart..." and "Shut down..." at the bottom, after the configured
//! entries. Choosing one swaps the three rows for a confirmation in place (the
//! question, disabled, then "Log out now" / "Restart now" / "Shut down now",
//! then "Cancel"), so the panel keeps its size and the action rows keep their
//! positions; only the confirmation row acts (`logind` ends the session,
//! `init` stops the machine).

use super::{Menu, Row};

/// What a session row does once confirmed: end the desktop session
/// (`logind`'s `Logout`) or stop the machine (`init`'s `PowerMode`; the shell
/// maps those to the generated constants).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Logout,
    PowerOff,
    Reboot,
}

impl Power {
    /// The first step's label.
    fn ask_label(self) -> &'static str {
        match self {
            Power::Logout => "Log out...",
            Power::Reboot => "Restart...",
            Power::PowerOff => "Shut down...",
        }
    }

    /// The question shown above the confirmation.
    fn question(self) -> &'static str {
        match self {
            Power::Logout => "End this session?",
            Power::Reboot => "Restart the computer?",
            Power::PowerOff => "Turn the computer off?",
        }
    }

    /// The confirmation's label.
    fn confirm_label(self) -> &'static str {
        match self {
            Power::Logout => "Log out now",
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
    /// A session row: show the confirmation.
    Ask(Power),
    /// The confirmation: end the session or stop the machine.
    Confirm(Power),
    /// Leave the confirmation.
    Cancel,
    /// The confirmation's question: a caption (disabled), never chosen.
    Question,
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
    /// Close the menu and act on this (the rows are back to their first step).
    Request(Power),
    /// Close the menu (the confirmation was cancelled).
    Close,
    /// Open the submenu of the category row chosen.
    Submenu(usize),
}

/// How many rows the session section always takes.
pub const POWER_ROWS: usize = 3;

fn row(label: &str, action: Action) -> Row {
    Row {
        app: String::new(),
        label: String::from(label),
        enabled: true,
        action,
    }
}

/// The three rows in their first step. "Log out..." goes on top so the power
/// rows keep the places they had before it existed.
pub(super) fn ask_rows() -> [Row; POWER_ROWS] {
    [Power::Logout, Power::Reboot, Power::PowerOff]
        .map(|power| row(power.ask_label(), Action::Ask(power)))
}

/// The three rows while `power` waits for confirmation: the question (a
/// caption, never chosen), the confirmation where "Restart..." was (so a
/// second click in place confirms a restart, as before), and "Cancel".
fn confirm_rows(power: Power) -> [Row; POWER_ROWS] {
    let question = Row {
        enabled: false,
        ..row(power.question(), Action::Question)
    };
    [
        question,
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
            Action::Confirm(power) => {
                // A refused logout leaves the desktop running: the next open
                // must show the first step again, not a stale confirmation.
                self.set_power_rows(ask_rows());
                Choice::Request(power)
            }
            Action::Cancel => {
                self.set_power_rows(ask_rows());
                Choice::Close
            }
            Action::Submenu(_) => Choice::Submenu(index),
            Action::Question => Choice::Nothing,
        }
    }

    /// Whether a confirmation is showing.
    pub fn confirming(&self) -> bool {
        self.rows
            .iter()
            .any(|row| matches!(row.action, Action::Confirm(_)))
    }

    /// Replace the last [`POWER_ROWS`] rows (always the session section).
    fn set_power_rows(&mut self, rows: [Row; POWER_ROWS]) {
        let start = self.rows.len().saturating_sub(POWER_ROWS);
        self.rows.truncate(start);
        self.rows.extend(rows);
    }
}
