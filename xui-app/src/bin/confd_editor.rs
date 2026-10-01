//! `xui-confd`: the Config app, a generic editor for the `confd` registry.
//!
//! The portable window lives in `crates/confd-editor`; this file supplies the
//! platform: a [`ConfdStore`] over `os.lazy.confd` that lists, reads and writes
//! the real configuration space.
//!
//! Serial evidence: `CONFDED:UP:PASS` after the first frame.

use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::platform::confd_store::ConfdStore;
use xui_confd_editor::app::{ConfdEditorApp, WINDOW};
use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;

fn main() -> std::process::ExitCode {
    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("CONFDED:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("CONFDED:UP:PASS"));

    // Match the desktop's light/dark mode and accent (Settings).
    let theme = backend.desktop_theme();
    let spec = PlatformSpec::new("Config").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, |ui| {
        if let Some(theme) = theme {
            ui.set_theme(theme);
        }
        match ConfdEditorApp::build(ui, Rc::new(ConfdStore::new())) {
            Ok(app) => app,
            Err(error) => {
                println!("CONFDED:BUILD:FAIL:{error}");
                std::process::exit(1);
            }
        }
    });
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("CONFDED:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
