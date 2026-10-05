//! `xui-files`: the spatial file explorer, migrated from xui's `xui-explorer`.
//!
//! The portable explorer core lives in `xui-explorer`; this file supplies the
//! LazyOS platform: `StdPlatform` over the Linux shim's `std::fs`, the
//! [`LazyLauncher`] (a `mimed.Open` client that launches through `init`), and
//! the start folder (a path on the command line, else the filesystem root `/`).
//!
//! Every folder opens its own window (a `xuid` surface); closing the last
//! window ends the process.
//!
//! Drag and drop (`docs/archiver-plan.md`): a press-and-drag on a window's
//! tiles offers the selection as a `text/uri-list`, and a `text/uri-list`
//! dropped on a window goes into its folder (`drop_into`), which then
//! refreshes. A drag between two Files windows *moves* what lives on the
//! folder's volume and copies the rest; Ctrl held at the drop copies and
//! Shift moves. A drop from another app copies unless Shift is held. Either
//! way nothing goes into itself, a clash gets `name (2)`, links stay links,
//! and an item dropped into its own folder is left alone.
//!
//! Serial evidence: `FILES:UP:PASS` after the first frame, `FILES:OPEN:PASS`
//! when `mimed` accepts a launch, `FILES:OPEN:REJECTED` when no app handles a
//! file, `FILES:DRAG:PASS:<n>` when a drag of `n` items starts,
//! `FILES:DROP:PASS:<done>:<failed>` after a drop (`done` counts copied and
//! moved items), then `FILES:DROP:MOVED:<moved>:COPIED:<copied>:SKIPPED:<n>`,
//! and `FILES:DROP:FAIL:<code>` when its paste is refused.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_app::backend::{DragOffer, DropEvent, LazyOSBackend};
use xui_app::platform::launcher::LazyLauncher;
use xui_app::platform::{argv, urilist};
use xui_app::themed::run_themed;
use xui_core::backend::{PlatformSpec, WindowId};
use xui_core::units::Dip;
use xui_explorer::platform::{Launcher, Platform};
use xui_explorer::std_platform::{drop_into, Intent, StdPlatform};
use xui_explorer::window::Msg;
use xui_explorer::Explorer;

/// Window size each folder window opens at.
const WINDOW: (i32, i32) = (720, 480);

/// The launcher, with serial reporting; it also passes the explorer's
/// open-origin hint on to the compositor so a folder window zooms open from the
/// tile that was double-clicked.
struct ReportingLauncher {
    backend: Rc<LazyOSBackend>,
}

impl Launcher for ReportingLauncher {
    fn open(&self, path: &Path) -> io::Result<()> {
        match LazyLauncher::new().open(path) {
            Ok(()) => {
                println!("FILES:OPEN:PASS:{}", path.display());
                Ok(())
            }
            Err(error) => {
                if error.kind() == io::ErrorKind::Unsupported {
                    println!("FILES:OPEN:REJECTED:{}", path.display());
                }
                Err(error)
            }
        }
    }

    fn hint_open_origin(&self, window: u64, tile: i32) {
        self.backend
            .hint_open_origin(WindowId::from_raw(window), tile);
    }
}

/// The payload of the latest drag this process offered: a drop carrying the
/// same bytes started in a Files window (the compositor never drops a drag
/// on its own source window, so it is another of ours).
type LastOffer = RefCell<Vec<u8>>;

/// A press-and-drag on a window's tiles: offer what it carries.
fn gesture(
    explorer: &Explorer,
    last: &LastOffer,
    window: WindowId,
    widget: xui_core::backend::WidgetId,
) -> Option<DragOffer> {
    let state = explorer.view_state(window.raw())?;
    let paths = state.drag_paths();
    if widget != state.view || paths.is_empty() {
        return None;
    }
    println!("FILES:DRAG:PASS:{}", paths.len());
    if state.collapsed() {
        let _ = state.proxy.send(Msg::RestoreSelection);
    }
    let bytes = urilist::encode(&paths).into_bytes();
    last.replace(bytes.clone());
    Some(DragOffer {
        mime: urilist::MIME.to_owned(),
        bytes,
    })
}

/// A drop on a window: copy or move the dropped paths into its folder.
fn dropped(
    explorer: &Explorer,
    backend: &LazyOSBackend,
    last: &LastOffer,
    window: WindowId,
    event: &DropEvent,
) {
    let DropEvent::Drop { mime, data, .. } = event else {
        return;
    };
    if mime != urilist::MIME {
        return;
    }
    let Some(state) = explorer.view_state(window.raw()) else {
        return;
    };
    match data {
        Ok(bytes) => {
            let held = backend.held_modifiers();
            let intent = Intent {
                ours: !bytes.is_empty() && *last.borrow() == *bytes,
                ctrl: held.ctrl,
                shift: held.shift,
            };
            let report = drop_into(&urilist::decode(bytes), &state.dir, intent);
            for (path, error) in &report.failed {
                println!("FILES:COPY:FAIL:{}:{error}", path.display());
            }
            println!(
                "FILES:DROP:PASS:{}:{}",
                report.copied + report.moved,
                report.failed.len()
            );
            println!(
                "FILES:DROP:MOVED:{}:COPIED:{}:SKIPPED:{}",
                report.moved, report.copied, report.skipped
            );
            // Every window refreshes: a move empties the source folder's
            // window too.
            explorer.refresh_all();
        }
        Err(code) => println!("FILES:DROP:FAIL:{code}"),
    }
}

/// The folder to start in: an argument, else the filesystem root `/` (Files
/// is the way to browse the whole volume, not just a home directory).
fn start_dir() -> PathBuf {
    argv::file_arg(std::env::args_os()).unwrap_or_else(|| PathBuf::from("/"))
}

fn main() -> std::process::ExitCode {
    let platform = Rc::new(StdPlatform::new());
    let start = start_dir();

    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("FILES:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let launcher = ReportingLauncher {
        backend: Rc::clone(&backend),
    };
    let explorer = Explorer::new(platform as Rc<dyn Platform>, Rc::new(launcher));
    let last: Rc<LastOffer> = Rc::default();
    {
        let (explorer, last) = (Rc::clone(&explorer), Rc::clone(&last));
        backend.on_drag_gesture(move |window, widget, _| gesture(&explorer, &last, window, widget));
    }
    {
        let explorer = Rc::clone(&explorer);
        // A weak handle: the backend owns this hook.
        let weak = Rc::downgrade(&backend);
        backend.on_drag_event(move |window, event| {
            if let Some(backend) = weak.upgrade() {
                dropped(&explorer, &backend, &last, window, event);
            }
        });
    }
    let (width, height) = backend.window_size(WINDOW);
    // Every folder window is resizable; the explorer's tile view re-flows.
    backend.set_size_hints(360, 240, 0, 0);
    backend.on_first_frame(|| println!("FILES:UP:PASS"));

    let spec = PlatformSpec::new("Files").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_themed(&backend, spec, move |ui| explorer.open_root(ui, start));
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("FILES:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
