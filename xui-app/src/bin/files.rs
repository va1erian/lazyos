//! `xui-files`: the explorer-style file manager, on xui's `xui-explorer`.
//!
//! The portable explorer core lives in `xui-explorer`; this file supplies the
//! LazyOS platform: `StdPlatform` over the Linux shim's `std::fs`, the
//! [`LazyLauncher`] (a `mimed.Open` client that launches through `init`),
//! the backend's keyboard focus (so Delete and Backspace edit the address
//! bar while it has the focus), and the start folder (a path on the command
//! line, else the filesystem root `/`).
//!
//! A window browses in place: opening a folder replaces the view, the
//! toolbar has Back, Forward, Up, the address bar, Sort, the folder's
//! Properties and the icons/details switch, and the title is the folder's
//! name. "Open in New Window" in the context menu opens another window (a
//! `xuid` surface); closing the last window ends the process.
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
//! Copy and paste (issue #488): Ctrl+C or the context menu's Copy offers the
//! selection on `clipboardd` as `text/uri-list`, and Ctrl+V or Paste copies
//! the newest such offer into the window's folder ([`session`]). The current
//! selection is published on `session/<id>/selection` (`idl/files.midl`).
//!
//! Reveal: an argument naming something that is not a folder (what `mimed`'s
//! `reveal` verb hands Files through `init.Launch`) opens the folder holding
//! it with that item selected.
//!
//! Serial evidence: `FILES:UP:PASS` after the first frame, `FILES:DIR:<path>`
//! when a window shows another folder,
//! `FILES:REVEAL:PASS:<path>` when a reveal selected its item
//! (`FILES:REVEAL:MISSING:<path>` when the folder does not hold it), the
//! copy, paste and selection markers of [`session`], `FILES:OPEN:PASS`
//! when `mimed` accepts a launch, `FILES:OPEN:REJECTED` when no app handles a
//! file, `FILES:DRAG:PASS:<n>` when a drag of `n` items starts,
//! `FILES:DROP:PASS:<done>:<failed>` after a drop (`done` counts copied and
//! moved items), then `FILES:DROP:MOVED:<moved>:COPIED:<copied>:SKIPPED:<n>`,
//! and `FILES:DROP:FAIL:<code>` when its paste is refused.

// A binary crate root in `src/bin/files.rs` resolves `mod session;` under
// `src/bin/`, so the path is spelled out.
#[path = "files/session.rs"]
mod session;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_app::backend::{DragOffer, DropEvent, LazyOSBackend};
use xui_app::launch;
use xui_app::platform::launcher::LazyLauncher;
use xui_app::platform::{argv, urilist};
use xui_core::backend::WindowId;
use xui_explorer::platform::{Launcher, Platform};
use xui_explorer::std_platform::{drop_into, Intent, StdPlatform};
use xui_explorer::window::Msg;
use xui_explorer::Explorer;

/// The size the first window opens at.
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

/// A drop on a window: copy or move the dropped paths into its folder. The
/// drag is "ours" when the compositor says it started in another Files
/// window of this process.
fn dropped(explorer: &Explorer, window: WindowId, event: &DropEvent) {
    let DropEvent::Drop {
        mime,
        data,
        modifiers,
        from_self,
        ..
    } = event
    else {
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
            let intent = Intent {
                ours: *from_self,
                ctrl: modifiers.ctrl,
                shift: modifiers.shift,
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

/// What the first window shows.
enum Start {
    /// A folder: the argument, else the filesystem root `/` (Files is the
    /// way to browse the whole volume, not just a home directory).
    Folder(PathBuf),
    /// The folder holding an item, with the item selected (a reveal).
    Reveal(PathBuf, OsString),
}

/// The start from the command line: a folder opens as itself, anything else
/// (a file, a link, a name that is gone) is revealed in its folder.
fn start() -> Start {
    let Some(path) = argv::file_arg(std::env::args_os()) else {
        return Start::Folder(PathBuf::from("/"));
    };
    let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir());
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) if !is_dir => Start::Reveal(dir.to_path_buf(), name.to_owned()),
        _ => Start::Folder(path),
    }
}

fn main() {
    let platform = Rc::new(StdPlatform::new());
    let start = start();

    launch::run("FILES", "Files", WINDOW, move |ui, backend| {
        let launcher = ReportingLauncher {
            backend: Rc::clone(backend),
        };
        let explorer = Explorer::with_session(
            platform as Rc<dyn Platform>,
            Rc::new(launcher),
            Rc::new(session::LazySession::new()),
        );
        {
            let backend = Rc::clone(backend);
            explorer.set_focus_probe(move || backend.focused());
        }
        {
            let explorer = Rc::clone(&explorer);
            backend.on_drag_gesture(move |window, widget, _| gesture(&explorer, window, widget));
        }
        {
            let explorer = Rc::clone(&explorer);
            backend.on_drag_event(move |window, event| dropped(&explorer, window, event));
        }
        // Every window is resizable; the toolbar and the views re-flow.
        backend.set_size_hints(360, 240, 0, 0);
        backend.on_first_frame(|| println!("FILES:UP:PASS"));
        Ok(match start {
            Start::Folder(dir) => explorer.open_root(ui, dir),
            Start::Reveal(dir, name) => {
                let path = dir.join(&name);
                let (window, found) = explorer.reveal_root(ui, dir, &name);
                if found {
                    println!("FILES:REVEAL:PASS:{}", path.display());
                } else {
                    println!("FILES:REVEAL:MISSING:{}", path.display());
                }
                window
            }
        })
    })
}
