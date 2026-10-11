//! LazySky's headless core (docs/lazysky-plan.md): everything a Bluesky
//! client does that is not drawing. No xui type, no LazyOS crate and no OS
//! call appears here; the front end supplies a [`Platform`] and drains
//! [`Events`]. `cargo tree -p lazysky-core` must list no `xui-*`.
//!
//! The core is driven by [`Request`]s and answers with [`Event`]s over
//! channels, so any front end (xui, a terminal, a test) can use it. Neither
//! [`Core::send`] nor [`Events::try_next`] ever blocks.

pub mod net;
pub mod platform;
pub mod proto;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

pub use net::{Http, HttpOptions, Response};
pub use platform::{Platform, Roots};

/// Installs `nettls-crypto` as the process's default rustls provider (the
/// GPLv2-compatible one, docs/tls-plan.md §3.2). Call it once, before any
/// thread starts; `false` means another provider was already installed. The
/// core's own TLS configuration names its provider explicitly, so this only
/// matters to other rustls users in the same process.
pub fn install_tls_provider() -> bool {
    rustls::crypto::CryptoProvider::install_default(nettls_crypto::provider()).is_ok()
}

/// What a front end asks for. Every request carries a `seq`, echoed in its
/// events, so a UI can drop answers for pages it has left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// One HTTPS (or HTTP) GET; the P0 proof of the headless path.
    HttpGet { seq: u64, url: String },
    /// Stops the worker threads.
    Shutdown,
}

/// What the core reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    HttpDone {
        seq: u64,
        status: u16,
        body: Vec<u8>,
    },
    Failed {
        seq: u64,
        reason: String,
    },
}

/// The core's handle: a request sender and the worker threads behind it.
pub struct Core {
    requests: Sender<Request>,
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
    pub fn wait(&self, timeout: std::time::Duration) -> Option<Event> {
        self.rx.recv_timeout(timeout).ok()
    }
}

impl Core {
    /// Starts the `api` thread, which owns the one HTTP client.
    pub fn start(platform: Arc<dyn Platform>) -> (Core, Events) {
        let (requests, request_rx) = mpsc::channel();
        let (event_tx, rx) = mpsc::channel();
        let api = thread::Builder::new()
            .name("sky-api".into())
            .spawn(move || api_thread(platform, request_rx, event_tx))
            .expect("the api thread starts");
        (
            Core {
                requests,
                api: Some(api),
            },
            Events { rx },
        )
    }

    /// Queues `request`; never blocks.
    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Shutdown);
        if let Some(api) = self.api.take() {
            let _ = api.join();
        }
    }
}

fn api_thread(platform: Arc<dyn Platform>, requests: Receiver<Request>, events: Sender<Event>) {
    // Made on this thread: its sockets are this thread's own.
    let http = Http::new(platform.ca_roots(), HttpOptions::default());
    while let Ok(request) = requests.recv() {
        match request {
            Request::Shutdown => break,
            Request::HttpGet { seq, url } => {
                let event = match http.get(&url, &[]) {
                    Ok(r) => Event::HttpDone {
                        seq,
                        status: r.status,
                        body: r.body,
                    },
                    Err(reason) => Event::Failed { seq, reason },
                };
                if events.send(event).is_err() {
                    break;
                }
            }
        }
    }
}
