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
//! Serial evidence: `PAINT:FONTS:<n>` (the Liberation faces read for the Text
//! tool), `PAINT:UP:PASS` after the first frame,
//! `PAINT:OPEN:PASS:<path>:<w>x<h>:<layers>` after a picture opens (PNG, JPEG,
//! BMP or OpenRaster) and `PAINT:SAVE:PASS:<path>:<format>` (`ora`, `png` or
//! `bmp`) after a successful save, both printed by the reporter the app calls
//! once the picture really is loaded or written; a failed write prints
//! `PAINT:SAVE:FAIL:<path>`. With the UI
//! probe on (`LAZYOS_UI_PROBE=1`) the app also prints `UI:WIDGET` lines for its
//! canvas, tool cells, swatches and palette (`tool:Pencil`, `swatch:primary`,
//! `palette:3`, ...) so a session clicks them by name.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_app::{font, launch};
use xui_app::platform::argv;
use xui_app::platform::clipboard as system_clipboard;
use xui_app::probe;
use xui_app::platform::storage::PngStorage;
use xui_core::widget::StdFileSystem;
use xui_paint::model::text::{CosmicRasterizer, TextRasterizer};
use xui_paint::storage::Storage;
use xui_paint::view::{PaintApp, Report, Setup, SystemClipboard};
use xui_paint::Msg;

/// Window size a compositor lays Paint out at.
const WINDOW: (i32, i32) = (800, 600);

/// The smallest window: the menu and options bars, the tool grid and the Colors
/// panel leave a canvas (and the colour editor) room to work in.
const MIN_WINDOW: (u32, u32) = (560, 420);

/// The MIME type Paint shares pictures as.
const PNG_MIME: &str = "image/png";

/// `clipboardd`'s `image/png` offers: a copy is offered when it fits the
/// service's inline cap (8 KiB), and a paste reads the newest offer. Without
/// the service (or the package's clipboard scopes) both calls fail quietly and
/// Paint keeps using its in-app slot.
struct PngClipboard;

impl SystemClipboard for PngClipboard {
    fn max_bytes(&self) -> usize {
        system_clipboard::MAX_BYTES
    }

    fn offer_png(&self, png: &[u8]) {
        let _ = system_clipboard::offer(PNG_MIME, png);
    }

    fn request_png(&self) -> Option<Vec<u8>> {
        // Token 0 asks for the newest offer in the caller's session.
        system_clipboard::paste(0, PNG_MIME).ok()
    }
}

/// The Text tool's rasterizer with the fonts this image has: the Liberation
/// Sans, Serif and Mono faces `fhs::share::FONTS_LIBERATION` holds (regular,
/// bold, italic and bold italic of each), then the Droid faces compiled into
/// every xui app. A missing directory just leaves the Droid faces.
fn text_rasterizer() -> Rc<dyn TextRasterizer> {
    let mut rasterizer = CosmicRasterizer::new();
    if let Ok(entries) = std::fs::read_dir(fhs::share::FONTS_LIBERATION) {
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "ttf"))
            .collect();
        // A stable order, so the first family registered (the stand-in for
        // an unknown name) does not depend on the directory's.
        files.sort();
        // Evidence that the text tool has the system fonts, not only Droid.
        println!("PAINT:FONTS:{}", files.len());
        for path in files {
            if let Ok(bytes) = std::fs::read(&path) {
                rasterizer.register_font(bytes);
            }
        }
    }
    for bytes in [font::BYTES, font::BOLD_BYTES, font::SERIF_BYTES] {
        rasterizer.register_font(bytes.to_vec());
    }
    Rc::new(rasterizer)
}

/// The default save file when no path is given.
fn default_path() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join("xpaint.png"),
        _ => PathBuf::from(fhs::mount::TMP).join("xpaint.png"),
    }
}

/// A storage wrapper that reports save/load outcomes on the serial log.
struct ReportingStorage {
    inner: PngStorage,
}

impl Storage for ReportingStorage {
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        match self.inner.save(bytes) {
            // The app reports a success, with the format it wrote.
            Ok(()) => Ok(()),
            Err(error) => {
                println!("PAINT:SAVE:FAIL:{}", self.inner.path().display());
                Err(error)
            }
        }
    }

    fn load(&self) -> Option<Vec<u8>> {
        self.inner.load()
    }

    fn available(&self) -> bool {
        self.inner.available()
    }

    fn save_to(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        let result = self.inner.save_to(path, bytes);
        if result.is_err() {
            println!("PAINT:SAVE:FAIL:{}", path.display());
        }
        result
    }

    fn load_from(&self, path: &Path) -> Option<Vec<u8>> {
        self.inner.load_from(path)
    }

    fn supports_paths(&self) -> bool {
        self.inner.supports_paths()
    }

    fn default_path(&self) -> Option<PathBuf> {
        self.inner.default_path()
    }
}

/// Prints the evidence line for a picture the app opened or saved. The path
/// is the store path, or the default file for a store with no paths.
fn announce(report: &Report, default: &Path) {
    match report {
        Report::Opened { path, size, layers } => {
            let path = path.as_deref().unwrap_or(default);
            println!(
                "PAINT:OPEN:PASS:{}:{}x{}:{layers}",
                path.display(),
                size.0,
                size.1
            );
        }
        Report::Saved { path, format } => {
            let path = path.as_deref().unwrap_or(default);
            println!("PAINT:SAVE:PASS:{}:{}", path.display(), format.extension());
        }
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

fn main() {
    let requested = argv::file_arg(std::env::args_os());
    let path = requested.clone().unwrap_or_else(default_path);
    let default_file = path.clone();
    let storage = Rc::new(ReportingStorage {
        inner: PngStorage::new(path),
    });

    launch::run("PAINT", "Paint", WINDOW, move |ui, backend| {
        // Paint's document is fixed; only the viewport follows the window, so
        // it can be resized down to a workable drawing area.
        backend.set_size_hints(MIN_WINDOW.0, MIN_WINDOW.1, 0, 0);
        backend.on_first_frame(|| println!("PAINT:UP:PASS"));
        let setup = Setup::new(storage)
            .with_files(Rc::new(StdFileSystem))
            .with_text(text_rasterizer());
        let mut app = PaintApp::build_with(ui, setup)?;
        app.set_start_dir(start_dir(requested.as_deref()));
        app.set_system_clipboard(Rc::new(PngClipboard));
        let default = default_file.clone();
        app.set_reporter(move |report| announce(report, &default));
        let startup = requested.is_some();
        let probing = probe::enabled();
        if probing {
            // Only a changed rectangle is printed, so the probe can keep
            // ticking for the whole session without flooding the serial log.
            let seen: RefCell<HashMap<String, (i32, i32, i32, i32)>> = RefCell::default();
            app.set_probe(move |window, name, rect| {
                let at = (rect.left, rect.top, rect.width(), rect.height());
                let key = format!("{window}
{name}");
                if seen.borrow().get(&key) != Some(&at) {
                    seen.borrow_mut().insert(key, at);
                    probe::widget(window, name, at.0, at.1, at.2, at.3);
                }
            });
        }
        if startup || probing {
            // A one-shot Open loads the file named on the command line, then
            // (with the probe on) a few slower ticks report the control
            // rectangles. The timer kills itself when done, so an idle Paint
            // costs no wakeups.
            let ticks = Rc::new(Cell::new(0u32));
            let ui_for_timer = ui.clone();
            ui.set_timer(if startup { 1 } else { 500 });
            ui.on_timer(move |id| {
                let tick = ticks.replace(ticks.get() + 1);
                if tick == 0 && startup {
                    ui_for_timer.kill_timer(id);
                    if probing {
                        ui_for_timer.set_timer(500);
                    }
                    return Some(Msg::OpenStartup);
                }
                if probing {
                    // The probe reports only what changed, so it keeps
                    // ticking: dialogs and the colour editor open later.
                    return Some(Msg::Probe);
                }
                ui_for_timer.kill_timer(id);
                None
            });
        }
        Ok(app)
    })
}
