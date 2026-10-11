//! LazySky as a desktop window on Windows, Linux or macOS, through xui's
//! winit backend: the same app the LazyOS binary launches.
//!
//! ```text
//! cargo run --release -p lazysky-ui --example desktop --features desktop
//! ```

use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use lazysky_core::{Platform, Roots, Settings};
use lazysky_ui::{LazySkyApp, Msg, WINDOW};
use xui_canvas::WinitBackend;

/// The desktop's answers to the core's questions.
struct Desktop;

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map_or_else(std::env::temp_dir, PathBuf::from)
}

impl Platform for Desktop {
    fn ca_roots(&self) -> Roots {
        match std::env::var_os("SSL_CERT_FILE").filter(|v| !v.is_empty()) {
            Some(path) => Roots::Bundle(PathBuf::from(path)),
            None => Roots::Native,
        }
    }

    fn app_dir(&self) -> PathBuf {
        if cfg!(windows) {
            let base = std::env::var_os("APPDATA").map_or_else(home, PathBuf::from);
            base.join("LazySky")
        } else if cfg!(target_os = "macos") {
            home().join("Library/Application Support/LazySky")
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map_or_else(|| home().join(".config"), PathBuf::from)
                .join("lazysky")
        }
    }

    fn pictures_dir(&self) -> PathBuf {
        home().join("Pictures")
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(format!("not a web link: {url}"));
        }
        let status = if cfg!(windows) {
            // `start` takes the first quoted argument as a window title.
            Command::new("cmd").args(["/c", "start", "", url]).status()
        } else if cfg!(target_os = "macos") {
            Command::new("open").arg(url).status()
        } else {
            Command::new("xdg-open").arg(url).status()
        };
        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("the opener exited with {s}")),
            Err(e) => Err(e.to_string()),
        }
    }

    fn log(&self, line: &str) {
        println!("{line}");
    }
}

fn main() {
    lazysky_core::install_tls_provider();
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
    let outcome = xui_core::app("LazySky")
        .size(WINDOW.0, WINDOW.1)
        .backend(Rc::new(WinitBackend::new()))
        .run(|ui| {
            LazySkyApp::build(ui, Arc::new(Desktop), Settings::default(), |msg| {
                if let Msg::Report(report) = msg {
                    println!("{report:?}");
                }
            })
        });
    if let Err(error) = outcome {
        eprintln!("lazysky: {error}");
        std::process::exit(1);
    }
}
