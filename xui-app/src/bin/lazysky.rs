//! `xui-lazysky`: LazySky, a Bluesky client (docs/lazysky-plan.md). The
//! protocol, session and images live in `crates/lazysky-core` and the window
//! in `crates/lazysky-ui`, both portable (the `desktop` example of
//! `lazysky-ui` runs the same app on Windows); this file only supplies the
//! LazyOS [`Platform`] and launches the window.
//!
//! `xui-lazysky [--service URL]`: `--service` points the login at another
//! Bluesky server (default: the `service` of `settings.toml` in
//! `$HOME/.apps/os.lazy.lazysky`, else `https://bsky.social`). `init` can pass
//! an app one absolute path only, so the harness writes the settings file.
//!
//! Serial evidence: `SKY:UP:PASS` after the first frame, `SKY:LOGIN:PASS:<handle>`
//! / `SKY:LOGIN:FAIL:<reason>` and `SKY:FEED:PAGE:<posts>:<skipped>` from the
//! core, `SKY:FEED:ROWS:<n>` as the timeline grows, `SKY:IMAGES:<n>` as pictures become
//! drawable, `SKY:PERF:...` frame times, `SKY:QUIT:PASS`. From
//! `launch::run`: `SKY:BIND:FAIL:<code>` and `SKY:RUN:FAIL:<error>`.

use std::path::PathBuf;
use std::sync::Arc;

use lazysky_core::{Platform, Roots, Settings};
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

/// The value of `--service`, when given.
fn service_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--service" {
            return args.next();
        }
        if let Some(url) = arg.strip_prefix("--service=") {
            return Some(url.to_string());
        }
    }
    None
}

fn main() {
    // `panic = "abort"`: a panic on any thread ends the app, so say where on
    // the serial line first (the default hook writes to a stderr nobody reads).
    std::panic::set_hook(Box::new(|info| {
        let place = info.location().map_or(String::new(), |l| format!(" at {}:{}", l.file(), l.line()));
        let text = info.payload().downcast_ref::<&str>().map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        println!("SKY:PANIC:{text}{place}");
    }));
    // The TLS provider is `nettls-crypto` (GPLv2-compatible), installed
    // before any thread starts, as Mail does.
    lazysky_core::install_tls_provider();
    let service = service_arg();
    launch::run("SKY", "LazySky", WINDOW, move |ui, backend| {
        backend.on_first_frame(|| println!("SKY:UP:PASS"));
        backend.set_size_hints(480, 360, 0, 0);
        // The user's settings (read once, before the first frame); a
        // `--service` argument wins for this run.
        let mut settings = Settings::load(&LazyOs.app_dir());
        if let Some(service) = &service {
            settings.service = service.clone();
        }
        LazySkyApp::build(ui, Arc::new(LazyOs), settings, |msg| match msg {
            Msg::Report(Report::FeedRows(n)) => println!("SKY:FEED:ROWS:{n}"),
            Msg::Report(Report::DetailRows(n)) => println!("SKY:DETAIL:ROWS:{n}"),
            Msg::Report(Report::Pictures(n)) => println!("SKY:IMAGES:{n}"),
            Msg::Report(Report::SignedOut) => println!("SKY:SIGNEDOUT"),
            Msg::Report(Report::ViewerReady(ms)) => println!("SKY:VIEWER:READY:{ms}"),
            Msg::Report(Report::ViewerWanted(what)) => println!("SKY:VIEWER:WANT:{what}"),
            Msg::Report(Report::ImageFailed(why)) => println!("SKY:IMAGE:FAIL:{why}"),
            Msg::Quit => println!("SKY:QUIT:PASS"),
            _ => {}
        })
        .inspect_err(|error| println!("SKY:BUILD:FAIL:{error}"))
    })
}
