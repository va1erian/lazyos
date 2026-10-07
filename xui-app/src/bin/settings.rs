//! `xui-settings`: the Settings app (a vertical section list on the left, the
//! active section on the right), migrated onto LazyOS as an ordinary xui app.
//!
//! The portable window lives in `crates/settings`; this file supplies the
//! platform: a [`ConfdStore`] over `os.lazy.confd` that persists to the data
//! volume, so `xuid` (theme, clock format) and `inputd` (keyboard layout)
//! pick changes up live, with the machine's `sys/**` keys written through
//! `elevd` ([`ElevatingStore`], an administrator approves); an [`OsSystem`]
//! over `timed`, `confd` and `sysinfo` for the Time & Date and About pages;
//! and [`OsAccounts`] for the Accounts page (docs/accounts-plan.md U1, U2).
//!
//! Serial evidence: `SETTINGS:UP:PASS` after the first frame;
//! `SETTINGS:BUILD:FAIL:<error>` when the window cannot be built (then
//! `SETTINGS:RUN:FAIL:<error>`), and `SETTINGS:BIND:FAIL:<errno>` without a
//! display.

use std::rc::Rc;

use xui_app::launch;
use xui_app::platform::accounts::OsAccounts;
use xui_app::platform::confd_store::ConfdStore;
use xui_app::platform::elevd::ElevatingStore;
use xui_app::platform::system::OsSystem;
use xui_settings::app::{SettingsApp, WINDOW};

fn main() {
    launch::run("SETTINGS", "Settings", WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("SETTINGS:UP:PASS"));
        SettingsApp::build(
            ui,
            Rc::new(ElevatingStore(ConfdStore::new())),
            Rc::new(OsSystem::new()),
            Rc::new(OsAccounts),
        )
        .inspect_err(|error| println!("SETTINGS:BUILD:FAIL:{error}"))
    })
}
