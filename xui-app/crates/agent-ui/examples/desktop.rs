//! LazyAgent as a desktop window on Windows, Linux or macOS, through xui's
//! winit backend: the same app the LazyOS binary launches.
//!
//! ```text
//! cargo run --release -p lazyagent-ui --example desktop --features desktop
//! ```
//!
//! The key is typed into the window (it is never read from the environment
//! here). Conversations live in the platform's config folder.

use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use lazyagent_core::{Platform, Roots};
use lazyagent_ui::{LazyAgentApp, Msg, WINDOW};
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
            base.join("LazyAgent")
        } else if cfg!(target_os = "macos") {
            home().join("Library/Application Support/LazyAgent")
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map_or_else(|| home().join(".config"), PathBuf::from)
                .join("lazyagent")
        }
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        if !(url.starts_with("https://") || url.starts_with("http://"))
            || url.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(format!("not a web link: {url}"));
        }
        let status = if cfg!(windows) {
            // Never through `cmd /c start`: cmd parses the line again, so a
            // link with `&` in it would run a second command.
            Command::new("rundll32")
                .args(["url.dll,FileProtocolHandler", url])
                .status()
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

    fn log(&self, _line: &str) {}
}

fn main() {
    lazyagent_core::install_tls_provider();
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::add_font(
        include_bytes!("../../../../assets/fonts/liberation/LiberationMono-Regular.ttf").to_vec(),
    );
    xui_canvas::set_default_family("Droid Sans");
    let outcome = xui_core::app("AI Agent")
        .size(WINDOW.0, WINDOW.1)
        .backend(Rc::new(WinitBackend::new()))
        .run(|ui| {
            LazyAgentApp::build(ui, Arc::new(Desktop), |msg| {
                if let Msg::Report(report) = msg {
                    println!("{report:?}");
                }
            })
        });
    if let Err(error) = outcome {
        eprintln!("lazyagent: {error}");
        std::process::exit(1);
    }
}
