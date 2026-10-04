//! LazyWeb's HTTP and HTTPS fetcher: what NetSurf calls to load every
//! `http:` and `https:` URL (`file:`, `data:` and `about:` it reads itself).
//!
//! The contract NetSurf expects, and why:
//!
//! - **No redirects.** A 3xx comes back as is, with its `Location`: NetSurf
//!   follows it itself, so the address bar, history and the new page's base
//!   URL all see the hop.
//! - **Decoded bodies.** We ask for `gzip, deflate` and decode here
//!   ([`decode`]), dropping `Content-Encoding` (and the now-wrong
//!   `Content-Length`): NetSurf's own decoding lived in its curl fetcher.
//! - **Off the engine thread.** NetSurf's engine thread must keep laying out
//!   and drawing while pages load; `xui-netsurf` already calls the fetcher
//!   on a new thread per request, which blocks there, at most
//!   [`Options::max_concurrent`] at once ([`pool`]).
//! - **Bounded.** Bodies are capped at [`Options::max_body`] decoded bytes and
//!   every network step has a timeout; an aborted fetch stops at the next
//!   chunk.
//!
//! The transport is ureq (HTTP/1.1) over std sockets, with TLS from rustls
//! and the in-tree `nettls-crypto` provider ([`tls`]), trusting only the
//! system CA bundle ([`roots`]). Connections are never pooled: on LazyOS each
//! thread has its own descriptor table, so a socket one fetch thread opened
//! is meaningless to the next.
//!
//! The fetcher talks to its caller through [`Sink`], which the NetSurf glue
//! ([`netsurf`]) implements for
//! `xui_netsurf::FetchResponder`, and the tests implement with a recorder.

mod decode;
pub mod netsurf;
mod pool;
mod resolve;
pub mod roots;
mod tls;
mod transfer;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub use pool::HttpFetcher;

/// The request method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
}

/// One request, as NetSurf hands it over.
#[derive(Debug, Clone)]
pub struct Request {
    /// An absolute `http:` or `https:` URL.
    pub url: String,
    pub method: Method,
    /// Headers NetSurf wants sent (`Accept`, `Referer`, `Cookie`,
    /// `Content-Type`, ...). Hop-by-hop headers and `Accept-Encoding` are the
    /// fetcher's own and are dropped.
    pub headers: Vec<(String, String)>,
    /// The POST body.
    pub body: Option<Vec<u8>>,
}

impl Request {
    /// A GET of `url` with no extra headers.
    pub fn get(url: &str) -> Request {
        Request {
            url: url.to_string(),
            method: Method::Get,
            headers: Vec::new(),
            body: None,
        }
    }
}

/// Where a fetch reports its response: `status`, then `header`s, then
/// `data` chunks, then exactly one of `finish` or `fail`.
pub trait Sink: Send + 'static {
    fn status(&self, code: u16);
    fn header(&self, name: &str, value: &str);
    fn data(&self, bytes: &[u8]);
    fn finish(self);
    fn fail(self, message: &str);
    /// Whether the caller no longer wants the response.
    fn is_aborted(&self) -> bool;
}

/// Where the trust anchors for `https:` come from.
#[derive(Debug, Clone)]
pub enum Roots {
    /// `SSL_CERT_FILE` when set, else the system bundle ([`roots::system_bundle`]).
    System,
    /// This PEM file.
    Bundle(PathBuf),
    /// These PEM bytes (tests).
    Pem(Arc<Vec<u8>>),
}

/// How the fetcher behaves; [`Options::default`] is what the browser uses.
#[derive(Debug, Clone)]
pub struct Options {
    pub roots: Roots,
    /// Fetches running at once; later ones wait for a free slot.
    pub max_concurrent: usize,
    /// The largest (decoded) body accepted; a larger one fails the fetch.
    pub max_body: u64,
    /// Name lookup plus TCP connect plus TLS handshake.
    pub connect_timeout: Duration,
    /// From the request being sent to the response headers.
    pub response_timeout: Duration,
    /// The whole body.
    pub body_timeout: Duration,
    /// Sent when the request carries no `User-Agent` of its own.
    pub user_agent: String,
}

/// 32 MiB: far more than any page, small enough that one bad server cannot
/// exhaust a LazyOS machine's memory.
pub const MAX_BODY: u64 = 32 * 1024 * 1024;

/// The browser's identity: NetSurf's own format, so sites that sniff for it
/// serve the simple pages it handles.
pub const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (LazyOS) NetSurf/3.11 LazyWeb/",
    env!("CARGO_PKG_VERSION")
);

impl Default for Options {
    fn default() -> Options {
        Options {
            roots: Roots::System,
            max_concurrent: 6,
            max_body: MAX_BODY,
            connect_timeout: Duration::from_secs(20),
            response_timeout: Duration::from_secs(30),
            body_timeout: Duration::from_secs(120),
            user_agent: USER_AGENT.to_string(),
        }
    }
}
