//! `xui-settings`: the Settings app (a vertical section list on the left, the
//! active section on the right), migrated onto LazyOS as an ordinary xui app.
//!
//! The portable window lives in `crates/settings`; this file supplies the
//! platform: a [`ConfdStore`] over `os.lazy.confd` that persists to the data
//! volume, so `xuid` (theme) and `inputd` (keyboard layout) pick changes up
//! live.
//!
//! Serial evidence: `SETTINGS:UP:PASS` after the first frame.

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::platform::confd_store::ConfdStore;
use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_settings::app::{SettingsApp, WINDOW};

fn main() -> std::process::ExitCode {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("SETTINGS:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("SETTINGS:UP:PASS"));

    let spec = PlatformSpec::new("Settings").size(Dip(width as f32), Dip(height as f32));
    let outcome =
        run_app(
            Rc::clone(&backend) as Rc<dyn Backend>,
            spec,
            |ui| match SettingsApp::build(ui, Rc::new(ConfdStore::new())) {
                Ok(app) => app,
                Err(error) => {
                    println!("SETTINGS:BUILD:FAIL:{error}");
                    std::process::exit(1);
                }
            },
        );
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("SETTINGS:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
