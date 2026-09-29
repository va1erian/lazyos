//! `xui-paint`: the Paint doodler, migrated from xui's `xui-paint`.
//!
//! The portable model, view and widgets live in `xui-paint`; this file supplies
//! the LazyOS platform: the backend, a PNG [`Storage`] at a fixed path (a file
//! named on the command line, else `$HOME/xpaint.png`) with atomic, symlink-
//! refusing writes, and the serial evidence markers the screenshot sessions
//! grep for.
//!
//! The portable `Storage` trait is path-less (the app calls `save`/`load` with
//! no argument), so the path is chosen once at start-up; a Save/Open inside the
//! app acts on that file.
//!
//! Serial evidence: `PAINT:UP:PASS` after the first frame, `PAINT:OPEN:PASS`
//! after a PNG loads, `PAINT:SAVE:PASS` after a successful save.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::platform::argv;
use xui_app::platform::storage::PngStorage;
use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_paint::storage::Storage;
use xui_paint::view::PaintApp;
use xui_paint::Msg;

/// Window size a compositor lays Paint out at.
const WINDOW: (i32, i32) = (800, 600);

/// The default save file when no path is given.
fn default_path() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join("xpaint.png"),
        _ => PathBuf::from("/tmp/xpaint.png"),
    }
}

/// A storage wrapper that reports save/load outcomes on the serial log.
struct ReportingStorage {
    inner: PngStorage,
}

impl Storage for ReportingStorage {
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        match self.inner.save(bytes) {
            Ok(()) => {
                println!("PAINT:SAVE:PASS:{}", self.inner.path().display());
                Ok(())
            }
            Err(error) => {
                println!("PAINT:SAVE:FAIL:{}", self.inner.path().display());
                Err(error)
            }
        }
    }

    fn load(&self) -> Option<Vec<u8>> {
        let bytes = self.inner.load();
        if bytes.is_some() {
            println!("PAINT:OPEN:PASS:{}", self.inner.path().display());
        }
        bytes
    }

    fn available(&self) -> bool {
        self.inner.available()
    }
}

fn main() -> std::process::ExitCode {
    let requested = argv::file_arg(std::env::args_os());
    let path = requested.clone().unwrap_or_else(default_path);
    let storage = Rc::new(ReportingStorage {
        inner: PngStorage::new(path),
    });

    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("PAINT:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("PAINT:UP:PASS"));

    let spec = PlatformSpec::new("Paint").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        let app = PaintApp::build(ui, storage.clone()).expect("the paint widgets built");
        if requested.is_some() {
            // A one-shot Open loads the file named on the command line. The
            // timer kills itself on first fire so an idle Paint costs no
            // wakeups.
            let fired = Rc::new(Cell::new(false));
            let ui_for_timer = ui.clone();
            ui.set_timer(1);
            ui.on_timer(move |id| {
                ui_for_timer.kill_timer(id);
                if fired.replace(true) {
                    None
                } else {
                    Some(Msg::Open)
                }
            });
        }
        app
    });
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("PAINT:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
