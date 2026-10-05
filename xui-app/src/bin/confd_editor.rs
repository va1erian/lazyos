//! `xui-confd`: the Config app, a generic editor for the `confd` registry.
//!
//! The portable window lives in `crates/confd-editor`; this file supplies the
//! platform: a [`ConfdStore`] over `os.lazy.confd` that lists, reads and writes
//! the real configuration space.
//!
//! Serial evidence: `CONFDED:UP:PASS` after the first frame;
//! `CONFDED:BUILD:FAIL:<error>` when the window cannot be built (then
//! `CONFDED:RUN:FAIL:<error>`), and `CONFDED:BIND:FAIL:<errno>` without a
//! display.

use std::rc::Rc;

use xui_app::launch;
use xui_app::platform::confd_store::ConfdStore;
use xui_confd_editor::app::{ConfdEditorApp, WINDOW};

fn main() {
    launch::run("CONFDED", "Config", WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("CONFDED:UP:PASS"));
        ConfdEditorApp::build(ui, Rc::new(ConfdStore::new()))
            .inspect_err(|error| println!("CONFDED:BUILD:FAIL:{error}"))
    })
}
