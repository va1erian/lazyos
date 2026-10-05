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
//! dropped on a window is copied into its folder (`copy_into`: never into
//! itself, `name (2)` on a clash, links as links), which then refreshes.
//!
//! Serial evidence: `FILES:UP:PASS` after the first frame, `FILES:OPEN:PASS`
//! when `mimed` accepts a launch, `FILES:OPEN:REJECTED` when no app handles a
//! file, `FILES:DRAG:PASS:<n>` when a drag of `n` items starts,
//! `FILES:DROP:PASS:<copied>:<failed>` after a drop is copied and
//! `FILES:DROP:FAIL:<code>` when its paste is refused.

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_app::backend::{DragOffer, DropEvent, LazyOSBackend};
use xui_app::launch;
use xui_app::platform::launcher::LazyLauncher;
use xui_app::platform::{argv, urilist};
use xui_core::backend::WindowId;
use xui_explorer::platform::{Launcher, Platform};
use xui_explorer::std_platform::{copy_into, StdPlatform};
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

/// A press-and-drag on a window's tiles: offer what it carries.
fn gesture(
    explorer: &Explorer,
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
    Some(DragOffer {
        mime: urilist::MIME.to_owned(),
        bytes: urilist::encode(&paths).into_bytes(),
    })
}

/// A drop on a window: copy the dropped paths into its folder.
fn dropped(explorer: &Explorer, window: WindowId, event: &DropEvent) {
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
            let report = copy_into(&urilist::decode(bytes), &state.dir);
            for (path, error) in &report.failed {
                println!("FILES:COPY:FAIL:{}:{error}", path.display());
            }
            println!("FILES:DROP:PASS:{}:{}", report.copied, report.failed.len());
            let _ = state.proxy.send(Msg::Refresh);
        }
        Err(code) => println!("FILES:DROP:FAIL:{code}"),
    }
}

/// The folder to start in: an argument, else the filesystem root `/` (Files
/// is the way to browse the whole volume, not just a home directory).
fn start_dir() -> PathBuf {
    argv::file_arg(std::env::args_os()).unwrap_or_else(|| PathBuf::from("/"))
}

fn main() {
    let platform = Rc::new(StdPlatform::new());
    let start = start_dir();

    launch::run("FILES", "Files", WINDOW, move |ui, backend| {
        let launcher = ReportingLauncher {
            backend: Rc::clone(backend),
        };
        let explorer = Explorer::new(platform as Rc<dyn Platform>, Rc::new(launcher));
        {
            let explorer = Rc::clone(&explorer);
            backend.on_drag_gesture(move |window, widget, _| gesture(&explorer, window, widget));
        }
        {
            let explorer = Rc::clone(&explorer);
            backend.on_drag_event(move |window, event| dropped(&explorer, window, event));
        }
        // Every folder window is resizable; the explorer's tile view re-flows.
        backend.set_size_hints(360, 240, 0, 0);
        backend.on_first_frame(|| println!("FILES:UP:PASS"));
        Ok(explorer.open_root(ui, start))
    })
}
