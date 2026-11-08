//! The worker that reads and parses documents off the UI thread: one thread for the life of the
//! window, so a burst of opens (holding `Alt+Left`, clicking down a list of links) never starts more
//! than one read at a time, and a request overtaken by a newer one before it starts is skipped.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use xui_core::app::Proxy;
use xui_docs::load_file;
use xui_markdown::Document;

use crate::app::Msg;

/// A document to read, and what the read is for.
pub struct Request {
    /// The load's number: the window shows only its newest.
    pub seq: u64,
    pub path: PathBuf,
    /// The heading to scroll to once shown.
    pub anchor: Option<String>,
    /// `back`, `forward` or `home` when Back, Forward or Home asked for it.
    pub nav: Option<&'static str>,
}

/// What the worker hands back with [`Msg::Loaded`]: the request and the document, or why it
/// could not be read.
pub struct Done {
    pub request: Request,
    pub result: Result<Arc<Document>, String>,
}

/// The handle the window keeps on the worker.
pub struct Loader {
    requests: Sender<Request>,
}

impl Loader {
    /// Starts the worker; it answers through `proxy` and ends with the window.
    pub fn start(proxy: Proxy<Msg>) -> io::Result<Loader> {
        let (requests, inbox) = channel();
        std::thread::Builder::new()
            .name("docs-load".into())
            .spawn(move || serve(&inbox, &proxy))?;
        Ok(Loader { requests })
    }

    /// Asks for `request`; `false` when the worker is gone.
    pub fn load(&self, request: Request) -> bool {
        self.requests.send(request).is_ok()
    }
}

/// Reads each request, newest first: requests queued behind it are dropped for the last of them.
fn serve(inbox: &Receiver<Request>, proxy: &Proxy<Msg>) {
    while let Ok(mut request) = inbox.recv() {
        while let Ok(newer) = inbox.try_recv() {
            request = newer;
        }
        let result = load_file(&request.path).map_err(|e| e.to_string());
        let done = Arc::new(Done { request, result });
        if proxy.send(Msg::Loaded(done)).is_err() {
            return;
        }
    }
}
