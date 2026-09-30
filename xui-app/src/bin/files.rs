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
//! Serial evidence: `FILES:UP:PASS` after the first frame, `FILES:OPEN:PASS`
//! when `mimed` accepts a launch, `FILES:OPEN:REJECTED` when no app handles a
//! file.

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_app::backend::LazyOSBackend;
use xui_app::platform::argv;
use xui_app::platform::files_fs::LazyPlatform;
use xui_app::platform::launcher::LazyLauncher;
use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;
use xui_explorer::platform::{Launcher, Platform};
use xui_explorer::std_platform::StdPlatform;
use xui_explorer::Explorer;

/// Window size each folder window opens at.
const WINDOW: (i32, i32) = (720, 480);

/// The launcher, with serial reporting.
struct ReportingLauncher;

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
}

/// The folder to start in: an argument, else the filesystem root `/` (Files
/// is the way to browse the whole volume, not just a home directory).
fn start_dir() -> PathBuf {
    argv::file_arg(std::env::args_os()).unwrap_or_else(|| PathBuf::from("/"))
}

fn main() -> std::process::ExitCode {
    let platform = Rc::new(LazyPlatform::new(StdPlatform::new()));
    let start = start_dir();
    let explorer = Explorer::new(platform as Rc<dyn Platform>, Rc::new(ReportingLauncher));

    let backend = match LazyOSBackend::connect() {
        Ok(backend) => Rc::new(backend),
        Err(code) => {
            println!("FILES:BIND:FAIL:{code}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (width, height) = backend.window_size(WINDOW);
    backend.on_first_frame(|| println!("FILES:UP:PASS"));

    let spec = PlatformSpec::new("Files").size(Dip(width as f32), Dip(height as f32));
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        explorer.open_root(ui, start)
    });
    backend.unbind();
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("FILES:RUN:FAIL:{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
