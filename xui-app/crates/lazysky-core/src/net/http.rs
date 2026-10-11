//! A blocking HTTP/1.1 client with the core's caps (docs/lazysky-plan.md §7.2).
//!
//! One [`Http`] belongs to one thread: on LazyOS a thread's sockets are its
//! own (`docs/tls-plan.md` §4.2), so an `Http` is not `Sync` on purpose and a
//! connection is never pooled or moved.

use std::io::{self, Read};
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use ureq::config::AutoHeaderValue;
use ureq::http;
use ureq::unversioned::transport::{Connector, TcpConnector};
use ureq::{Agent, Error};

use super::resolve::InlineResolver;
use super::tls::{self, LazyConfig, TlsConnector};
use crate::platform::Roots;

/// Request headers the client owns: connection management, framing and
/// content coding are not the caller's to set.
const OWN_HEADERS: &[&str] = &[
    "accept-encoding",
    "connection",
    "content-length",
    "host",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

#[derive(Clone, Debug)]
pub struct HttpOptions {
    pub user_agent: String,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    /// A body longer than this is an error, not a truncation.
    pub max_body: usize,
}

impl Default for HttpOptions {
    fn default() -> HttpOptions {
        HttpOptions {
            user_agent: concat!("LazySky/", env!("CARGO_PKG_VERSION")).to_string(),
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(30),
            max_body: 8 * 1024 * 1024,
        }
    }
}

/// A complete response (any status).
#[derive(Debug, Clone, Default)]
pub struct Response {
    pub status: u16,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub struct Http {
    agent: Agent,
    options: HttpOptions,
    /// Keeps `Http` on the thread that made it.
    _thread: PhantomData<*const ()>,
}

impl Http {
    pub fn new(roots: Roots, options: HttpOptions) -> Http {
        let tls = Arc::new(LazyConfig::new(roots));
        let connector = ().chain(TcpConnector::default()).chain(TlsConnector::new(tls));
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            // Never from the environment: a stray variable must not reroute
            // the client's traffic.
            .proxy(None)
            .user_agent(AutoHeaderValue::None)
            .accept_encoding(AutoHeaderValue::None)
            .timeout_connect(Some(options.connect_timeout))
            .timeout_send_request(Some(options.read_timeout))
            .timeout_send_body(Some(options.read_timeout))
            .timeout_recv_response(Some(options.read_timeout))
            .timeout_recv_body(Some(options.read_timeout))
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .build();
        Http {
            agent: Agent::with_parts(config, connector, InlineResolver),
            options,
            _thread: PhantomData,
        }
    }

    /// `GET url` with extra `headers`.
    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Response, String> {
        self.send(http::Method::GET, url, headers, None)
    }

    /// `POST url` with `body` (its `Content-Type` is in `headers`).
    pub fn post(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Response, String> {
        self.send(http::Method::POST, url, headers, Some(body))
    }

    fn send(
        &self,
        method: http::Method,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<Response, String> {
        let host = host_of(url);
        let secure = url
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"));
        let plain = url
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("http://"));
        if !secure && !plain {
            return Err(format!("cannot fetch {url} (only http and https)"));
        }
        let mut builder = http::Request::builder().method(method).uri(url);
        let mut has_agent = false;
        for (name, value) in headers {
            let lower = name.to_ascii_lowercase();
            if OWN_HEADERS.contains(&lower.as_str()) {
                continue;
            }
            has_agent |= lower == "user-agent";
            builder = builder.header(*name, *value);
        }
        if !has_agent {
            builder = builder.header("user-agent", self.options.user_agent.as_str());
        }
        let bad = |e: http::Error| format!("not a usable request for {url}: {e}");
        let result = match body {
            Some(bytes) => self.agent.run(builder.body(bytes).map_err(bad)?),
            None => self.agent.run(builder.body(()).map_err(bad)?),
        };
        let response = result.map_err(|e| describe(e, &host))?;
        let (parts, mut body) = response.into_parts();
        let mut bytes = Vec::new();
        // One byte past the cap tells "exactly at the cap" from "over it".
        let cap = self.options.max_body as u64;
        body.as_reader()
            .take(cap + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("{host}: reading the answer failed: {e}"))?;
        if bytes.len() as u64 > cap {
            return Err(format!("{host}: the answer is longer than {cap} bytes"));
        }
        let headers = parts
            .headers
            .iter()
            .filter_map(|(n, v)| Some((n.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        Ok(Response {
            status: parts.status.as_u16(),
            headers,
            body: bytes,
        })
    }
}

/// The `host[:port]` of `url`, for messages.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_string()
}

/// Why a request got no response, in words a status line can show.
fn describe(error: Error, host: &str) -> String {
    match error {
        Error::HostNotFound => format!("cannot find the server {host}"),
        Error::ConnectionFailed => format!("cannot connect to {host}"),
        Error::Timeout(stage) => format!("{host} did not answer in time ({stage})"),
        Error::Tls(what) => tls::take_failure().unwrap_or_else(|| format!("{host}: {what}")),
        Error::BadUri(uri) => format!("not a usable URL: {uri}"),
        Error::Io(e) => match e.kind() {
            io::ErrorKind::ConnectionRefused => format!("{host} refused the connection"),
            io::ErrorKind::TimedOut => format!("{host} did not answer in time"),
            _ => format!("{host}: {e}"),
        },
        other => format!("{host}: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_named_for_messages() {
        assert_eq!(host_of("https://bsky.social/xrpc/x?a=b"), "bsky.social");
        assert_eq!(host_of("http://127.0.0.1:8080"), "127.0.0.1:8080");
    }

    #[test]
    fn other_schemes_are_refused_before_the_network() {
        let http = Http::new(Roots::Pem(Arc::default()), HttpOptions::default());
        let err = http.get("ftp://example.org/x", &[]).unwrap_err();
        assert!(err.contains("only http and https"), "{err}");
    }
}
