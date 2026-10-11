//! Headless LazySky: sign in and print the timeline through the [`Core`]
//! API, with no UI. Proves the core runs on its own.
//!
//! ```text
//! LAZYSKY_HANDLE=you.bsky.social LAZYSKY_PASSWORD=xxxx-xxxx-xxxx-xxxx \
//!     cargo run -p lazysky-core --example sky_cli --features native-roots
//! ```
//!
//! `LAZYSKY_SERVICE` names another login service. Without credentials it
//! only checks the service's `_health` endpoint over HTTPS.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lazysky_core::proto::{Embed, Reason};
use lazysky_core::{
    Core, Event, FeedSource, Http, HttpOptions, Platform, Request, Roots, Secret, Settings,
};

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

fn wait(events: &lazysky_core::Events) -> Event {
    loop {
        match events.wait(Duration::from_secs(60)) {
            Some(Event::Status { line }) => eprintln!("{line}"),
            Some(other) => return other,
            None => {
                eprintln!("no answer in 60 s");
                std::process::exit(1);
            }
        }
    }
}

fn main() {
    let mut settings = Settings::default();
    if let Ok(service) = std::env::var("LAZYSKY_SERVICE") {
        settings.service = service;
    }
    let (Ok(handle), Ok(password)) = (
        std::env::var("LAZYSKY_HANDLE"),
        std::env::var("LAZYSKY_PASSWORD"),
    ) else {
        let http = Http::new(Terminal.ca_roots(), HttpOptions::default());
        let url = format!("{}/xrpc/_health", settings.service);
        match http.get(&url, &[]) {
            Ok(r) => println!("{url}: HTTP {}, {} bytes", r.status, r.body.len()),
            Err(why) => {
                eprintln!("{url}: {why}");
                std::process::exit(1);
            }
        }
        return;
    };
    let (core, events) = Core::start(Arc::new(Terminal), settings);
    core.send(Request::Login {
        seq: 1,
        handle,
        password: Secret::new(password),
    });
    match wait(&events) {
        Event::LoggedIn { account, .. } => println!("signed in as {}", account.handle),
        Event::LoginFailed { reason, .. } => {
            eprintln!("sign-in failed: {reason}");
            std::process::exit(1);
        }
        other => panic!("unexpected {other:?}"),
    }
    core.send(Request::Feed {
        seq: 2,
        source: FeedSource::Timeline,
        cursor: None,
    });
    match wait(&events) {
        Event::Feed { page, .. } => {
            for item in &page.items {
                let post = &item.post;
                let by = match &item.reason {
                    Some(Reason::Repost { by }) => format!(" (reposted by {})", by.name()),
                    None => String::new(),
                };
                let pictures = match post.embed.images().len() {
                    0 => String::new(),
                    n => format!(" [{n} picture(s)]"),
                };
                let quote = matches!(post.embed, Embed::Record(_) | Embed::RecordWithMedia(..));
                let first_line = post.text.lines().next().unwrap_or("");
                println!(
                    "{}{by}: {first_line}{pictures}{}",
                    post.author.name(),
                    if quote { " [quote]" } else { "" }
                );
            }
            println!("{} posts, {} skipped", page.items.len(), page.skipped);
        }
        Event::Failed { reason, .. } => {
            eprintln!("timeline failed: {reason}");
            std::process::exit(1);
        }
        other => panic!("unexpected {other:?}"),
    }
}
