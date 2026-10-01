//! The desktop menu's power rows (docs/shutdown.md, stage S-c): "Restart..."
//! and "Shut down..." after the apps. Choosing one turns the menu into a
//! confirmation ("Shut down now" / "Cancel"); confirming raises the
//! shutting-down overlay and asks `init` for an orderly stop. `init` does the
//! rest, and its `system/power/state` topic keeps the overlay up.
//!
//! The rows travel through the menu as ordinary [`Entry`] values whose app id
//! starts with `@`, which no registry id can (ids are lowercase names), so the
//! menu tells them apart without a second list type.

use alloc::string::String;
use alloc::vec::Vec;
use deskmenu::Entry;
use user::messenger::services::{self, INIT_NAME, POWER_MODE_POWER_OFF, POWER_MODE_REBOOT};
use user::sys;

use super::{menuitems, powerfeed};

const REBOOT: &str = "@reboot";
const POWEROFF: &str = "@poweroff";
const CONFIRM_REBOOT: &str = "@reboot!";
const CONFIRM_POWEROFF: &str = "@poweroff!";
const CANCEL: &str = "@cancel";
/// How long the compositor waits for `init`'s answer (100 Hz): it answers
/// before stopping anything, so this is a backstop.
const REQUEST_TICKS: u64 = 300;

/// What the menu does after a row was chosen.
pub(super) enum Outcome {
    /// The menu changed (the confirmation) and stays open.
    KeepOpen,
    /// Close the menu.
    Close,
    /// Close it and repaint everything (the overlay went up).
    CloseAndRepaint,
}

fn entry(app: &str, label: &str) -> Entry {
    Entry {
        app: String::from(app),
        label: String::from(label),
    }
}

/// The rows appended after the apps.
pub(super) fn entries() -> [Entry; 2] {
    [entry(REBOOT, "Restart..."), entry(POWEROFF, "Shut down...")]
}

/// Whether `app` is one of these rows rather than a registry app.
pub(super) fn is_power_row(app: &str) -> bool {
    app.starts_with('@')
}

/// Act on a chosen power row.
pub(super) fn activate(app: &str) -> Outcome {
    match app {
        REBOOT => confirm(CONFIRM_REBOOT, "Restart now"),
        POWEROFF => confirm(CONFIRM_POWEROFF, "Shut down now"),
        CONFIRM_REBOOT => request(POWER_MODE_REBOOT),
        CONFIRM_POWEROFF => request(POWER_MODE_POWER_OFF),
        _ => Outcome::Close, // `CANCEL`
    }
}

/// Swap the menu's rows for the confirmation.
fn confirm(app: &str, label: &str) -> Outcome {
    let rows: Vec<Entry> = alloc::vec![entry(app, label), entry(CANCEL, "Cancel")];
    menuitems::set_confirm(rows);
    sys::write_str("XUID:POWER:CONFIRM\n");
    Outcome::KeepOpen
}

/// Raise the overlay and ask `init` to stop the machine with `mode`. A
/// refusal (or no `init`) takes the overlay down again.
fn request(mode: u32) -> Outcome {
    powerfeed::show(mode);
    let deadline = Some(sys::clock() + REQUEST_TICKS);
    let result = services::resolve_service(INIT_NAME).and_then(|init| {
        services::shutdown(&init, mode, "requested from the desktop menu", false, deadline)
    });
    match result {
        Ok(phase) => {
            sys::write_str(&alloc::format!("XUID:POWER:REQUEST mode={mode} phase={phase}\n"));
        }
        Err(error) => {
            powerfeed::hide();
            sys::write_str(&alloc::format!(
                "XUID:POWER:REQUEST:FAIL mode={mode} errno={}\n",
                error.errno().unwrap_or(0)
            ));
        }
    }
    Outcome::CloseAndRepaint
}
