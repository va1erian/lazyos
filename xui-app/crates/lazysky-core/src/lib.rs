//! LazySky's headless core (docs/lazysky-plan.md): everything a Bluesky
//! client does that is not drawing. No xui type, no LazyOS crate and no OS
//! call appears here; the front end supplies a [`Platform`] and drains
//! [`Events`]. `cargo tree -p lazysky-core` must list no `xui-*`.
//!
//! The core is driven by [`Request`]s and answers with [`Event`]s over
//! channels, so any front end (xui, a terminal, a test) can use it. Neither
//! [`Core::send`] nor [`Events::try_next`] ever blocks.

pub mod feed;
pub mod net;
pub mod platform;
pub mod proto;
mod worker;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub use net::{Http, HttpOptions, Response};
pub use platform::{Platform, Roots};
pub use proto::{Page, Post};

/// Installs `nettls-crypto` as the process's default rustls provider (the
/// GPLv2-compatible one, docs/tls-plan.md §3.2). Call it once, before any
/// thread starts; `false` means another provider was already installed. The
/// core's own TLS configuration names its provider explicitly, so this only
/// matters to other rustls users in the same process.
pub fn install_tls_provider() -> bool {
    rustls::crypto::CryptoProvider::install_default(nettls_crypto::provider()).is_ok()
}

/// What the user can change (more of it arrives with the settings page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The login service; the account's PDS is read from its answer.
    pub service: String,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            service: "https://bsky.social".to_string(),
        }
    }
}

/// A password. It has no `Debug` output of its own worth reading and is
/// never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Which list of posts a [`Request::Feed`] fetches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedSource {
    /// The signed-in user's home timeline.
    Timeline,
}

/// What a front end asks for. Every request carries a `seq`, echoed in its
/// events, so a UI can drop answers for pages it has left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Signs in with a handle and an app password.
    Login {
        seq: u64,
        handle: String,
        password: Secret,
    },
    /// Forgets the session.
    Logout { seq: u64 },
    /// One page of a feed, from `cursor` (or the newest).
    Feed {
        seq: u64,
        source: FeedSource,
        cursor: Option<String>,
    },
}

/// The signed-in account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub did: String,
    pub handle: String,
}

/// What the core reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    LoggedIn {
        seq: u64,
        account: Account,
    },
    LoginFailed {
        seq: u64,
        reason: String,
    },
    LoggedOut {
        seq: u64,
    },
    /// A page of a feed. `cursor` is the one the request carried.
    Feed {
        seq: u64,
        source: FeedSource,
        cursor: Option<String>,
        page: Page,
    },
    /// A request failed. `signed_out` says the session is gone for good.
    Failed {
        seq: u64,
        reason: String,
        signed_out: bool,
    },
    /// Something worth a status line that is not an answer.
    Status {
        line: String,
    },
}

/// The core's handle: a request sender and the worker threads behind it.
pub struct Core {
    requests: Sender<worker::Message>,
    closing: Arc<AtomicBool>,
    api: Option<thread::JoinHandle<()>>,
}

/// The receiving end of the core's events.
pub struct Events {
    rx: Receiver<Event>,
}

impl Events {
    /// The next event, or `None` when nothing is waiting.
    pub fn try_next(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    /// Waits up to `timeout` for the next event (tests and the CLI only;
    /// never call it from a UI thread).
    pub fn wait(&self, timeout: Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
    }
}

impl Core {
    /// Starts the `api` thread, which owns the one HTTP client and the
    /// session.
    pub fn start(platform: Arc<dyn Platform>, settings: Settings) -> (Core, Events) {
        let (requests, request_rx) = mpsc::channel();
        let (event_tx, rx) = mpsc::channel();
        let closing = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&closing);
        let api = thread::Builder::new()
            .name("sky-api".into())
            .spawn(move || worker::run(platform, settings, request_rx, event_tx, flag))
            .expect("the api thread starts");
        (
            Core {
                requests,
                closing,
                api: Some(api),
            },
            Events { rx },
        )
    }

    /// Queues `request`; never blocks.
    pub fn send(&self, request: Request) {
        let _ = self.requests.send(worker::Message::Request(request));
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.requests.send(worker::Message::Shutdown);
        if let Some(api) = self.api.take() {
            let _ = api.join();
        }
    }
}
