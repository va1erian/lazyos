//! Headless LazySky: one HTTPS GET through the [`Core`] API, printing the
//! status and the first bytes. Proves the core runs with no UI.
//!
//! ```text
//! cargo run -p lazysky-core --example sky_cli --features native-roots -- https://bsky.social/xrpc/_health
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lazysky_core::{Core, Event, Platform, Request, Roots};

struct Terminal;

impl Platform for Terminal {
    fn ca_roots(&self) -> Roots {
        match std::env::var_os("SSL_CERT_FILE").filter(|v| !v.is_empty()) {
            Some(path) => Roots::Bundle(PathBuf::from(path)),
            None => Roots::Native,
        }
    }

    fn app_dir(&self) -> PathBuf {
        std::env::temp_dir().join("lazysky")
    }

    fn pictures_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        Err(format!("sky_cli does not open {url}"))
    }

    fn log(&self, line: &str) {
        println!("{line}");
    }
}

fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://bsky.social/xrpc/_health".to_string());
    let (core, events) = Core::start(Arc::new(Terminal));
    core.send(Request::HttpGet { seq: 1, url });
    match events.wait(Duration::from_secs(60)) {
        Some(Event::HttpDone { status, body, .. }) => {
            let shown = String::from_utf8_lossy(&body[..body.len().min(200)]);
            println!("status {status}, {} bytes: {shown}", body.len());
        }
        Some(Event::Failed { reason, .. }) => {
            eprintln!("failed: {reason}");
            std::process::exit(1);
        }
        None => {
            eprintln!("no answer in 60 s");
            std::process::exit(1);
        }
    }
}
