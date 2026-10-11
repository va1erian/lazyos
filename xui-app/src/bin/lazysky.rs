//! `xui-lazysky`: LazySky, a Bluesky client (docs/lazysky-plan.md). The
//! protocol, session and images live in `crates/lazysky-core` and the window
//! in `crates/lazysky-ui`, both portable (the `desktop` example of
//! `lazysky-ui` runs the same app on Windows); this file only supplies the
//! LazyOS [`Platform`] and launches the window.
//!
//! Serial evidence: `SKY:UP:PASS` after the first frame and
//! `SKY:SPIKE:SHAPE:PASS:<height>` once text shaped on a worker thread was
//! delivered to the UI thread (P0). From `launch::run`: `SKY:BIND:FAIL:<code>`
//! and `SKY:RUN:FAIL:<error>`.

use std::path::PathBuf;
use std::sync::Arc;

use lazysky_core::{Platform, Roots};
use lazysky_ui::{LazySkyApp, Msg, Report, WINDOW};
use xui_app::launch;
use xui_app::platform::{dirs, launcher};

/// The app's id: its data folder is `$HOME/.apps/<SYSTEM_NAME>`.
const SYSTEM_NAME: &str = "os.lazy.lazysky";
/// The pictures folder in the home.
const PICTURES_DIR: &str = "Pictures";

/// LazyOS's answers to the core's questions: the CA bundle from `fhs`, the
/// app's folder under `$HOME/.apps`, links through `mimed`.
struct LazyOs;

impl Platform for LazyOs {
    fn ca_roots(&self) -> Roots {
        let from_env = std::env::var_os("SSL_CERT_FILE").filter(|v| !v.is_empty());
        let path = from_env.map(PathBuf::from).unwrap_or_else(|| {
            [fhs::etc::CA_BUNDLE, fhs::etc::LINUX_CA_BUNDLE]
                .iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
                .unwrap_or_else(|| PathBuf::from(fhs::etc::CA_BUNDLE))
        });
        Roots::Bundle(path)
    }

    fn app_dir(&self) -> PathBuf {
        let home = dirs::default_dir();
        PathBuf::from(fhs::app_data_dir(&home.to_string_lossy(), SYSTEM_NAME))
    }

    fn pictures_dir(&self) -> PathBuf {
        let pictures = dirs::default_dir().join(PICTURES_DIR);
        if pictures.is_dir() {
            pictures
        } else {
            dirs::default_dir()
        }
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        launcher::open_url(url).map_err(|e| e.to_string())
    }

    fn log(&self, line: &str) {
        println!("{line}");
    }
}

fn main() {
    // The TLS provider is `nettls-crypto` (GPLv2-compatible), installed
    // before any thread starts, as Mail does.
    lazysky_core::install_tls_provider();
    launch::run("SKY", "LazySky", WINDOW, move |ui, backend| {
        backend.on_first_frame(|| println!("SKY:UP:PASS"));
        backend.set_size_hints(480, 360, 0, 0);
        LazySkyApp::build(ui, Arc::new(LazyOs), |msg| match msg {
            Msg::Report(Report::ShapedOffThread { lines_height }) => {
                println!("SKY:SPIKE:SHAPE:PASS:{lines_height}");
            }
            Msg::Quit => println!("SKY:QUIT:PASS"),
            // A timer tick the window handles itself; never reported.
            Msg::Poll => {}
        })
        .inspect_err(|error| println!("SKY:BUILD:FAIL:{error}"))
    })
}
