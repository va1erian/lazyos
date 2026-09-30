//! `xui-paint`: the Paint doodler, migrated from xui's `xui-paint`.
//!
//! The portable model, view and widgets live in `xui-paint`; this file supplies
//! the LazyOS platform: the backend, a PNG [`Storage`] at a fixed path (a file
//! named on the command line, else `$HOME/xpaint.png`) with atomic, symlink-
//! refusing writes, and the serial evidence markers the screenshot sessions
//! grep for.
//!
//! The path-less `Storage::save`/`load` act on the start-up file; Open and Save
//! As go through the xui file dialogs (`LazyFileSystem`) and the path seam
//! (`save_to`/`load_from`), starting in the file's directory, `$HOME` or `/`.
//!
//! Serial evidence: `PAINT:UP:PASS` after the first frame, `PAINT:OPEN:PASS`
//! after a PNG loads, `PAINT:SAVE:PASS` after a successful save.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::platform::argv;
use xui_app::platform::dialog_fs::LazyFileSystem;
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

    fn save_to(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        let result = self.inner.save_to(path, bytes);
        let verdict = if result.is_ok() { "PASS" } else { "FAIL" };
        println!("PAINT:SAVE:{verdict}:{}", path.display());
        result
    }

    fn load_from(&self, path: &Path) -> Option<Vec<u8>> {
        let bytes = self.inner.load_from(path);
        if bytes.is_some() {
            println!("PAINT:OPEN:PASS:{}", path.display());
        }
        bytes
    }

    fn supports_paths(&self) -> bool {
        self.inner.supports_paths()
    }

    fn default_path(&self) -> Option<PathBuf> {
        self.inner.default_path()
    }
}

/// Where the file dialogs start: the file's directory, else `$HOME`, else `/`.
fn start_dir(requested: Option<&Path>) -> PathBuf {
    requested
        .and_then(Path::parent)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("/"))
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
    // Paint's document is fixed; only the viewport follows the window, so it
    // can be resized down to a workable drawing area.
    backend.set_size_hints(320, 240, 0, 0);
    backend.on_first_frame(|| println!("PAINT:UP:PASS"));

    let spec = PlatformSpec::new("Paint").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        let app = PaintApp::build_with_files(ui, storage.clone(), LazyFileSystem::shared())
            .expect("the paint widgets built");
        app.set_start_dir(start_dir(requested.as_deref()));
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
                    Some(Msg::OpenStartup)
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
