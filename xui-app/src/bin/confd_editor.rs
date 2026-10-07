//! `xui-confd`: the Config app, a generic editor for the `confd` registry.
//!
//! The portable window lives in `crates/confd-editor`; this file supplies the
//! platform: a [`ConfdStore`] over `os.lazy.confd` that lists, reads and writes
//! the user's own keys (`user/<uid>/**`), and **Elevate** ([`ConfElevation`]),
//! after which every key is read and written through `elevd` once an
//! administrator approved (docs/accounts-plan.md U2).
//!
//! Serial evidence: `CONFDED:UP:PASS` after the first frame;
//! `CONFDED:BUILD:FAIL:<error>` when the window cannot be built (then
//! `CONFDED:RUN:FAIL:<error>`), and `CONFDED:BIND:FAIL:<errno>` without a
//! display.

use std::rc::Rc;

use xui_app::launch;
use xui_app::platform::confd_store::ConfdStore;
use xui_app::platform::elevd::ConfElevation;
use xui_confd_editor::app::{ConfdEditorApp, WINDOW};
use xui_confd_editor::store::Scope;
use xui_settings::store::ConfigStore;

fn main() {
    launch::run("CONFDED", "Config", WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("CONFDED:UP:PASS"));
        // The user's own keys until Elevate (docs/accounts-plan.md U2).
        let scope = match ConfigStore::uid(&ConfdStore::new()) {
            Some(uid) => Scope::own(uid, Rc::new(ConfElevation)),
            None => Scope::everything(),
        };
        ConfdEditorApp::build(ui, Rc::new(ConfdStore::new()), scope)
            .inspect_err(|error| println!("CONFDED:BUILD:FAIL:{error}"))
    })
}
