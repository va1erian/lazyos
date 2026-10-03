//! The HTTP exchange: one `ureq` agent with our resolver and TLS connector,
//! and the redirect loop.
//!
//! ureq's own redirect following is off (`max_redirects(0)`): every hop
//! comes back here so [`crate::redirect`]'s rules (no https -> http, the
//! limit) apply and each hop can be reported.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::ClientConfig;
use ureq::http::Response;
use ureq::unversioned::transport::{Connector, TcpConnector};
use ureq::{Agent, Body};
use url::Url;

use crate::opts::Options;
use crate::redirect;
use crate::report::{Failure, TlsFailure};
use crate::resolve::SyncResolver;
use crate::sanitize::printable;
use crate::tls::{TlsConnector, TlsLog};

/// The User-Agent sent unless the user names one.
pub const USER_AGENT: &str = concat!("LazyOS-fetch/", env!("CARGO_PKG_VERSION"));

/// Told about each hop as it happens (progress, `-v`, `-S`).
pub trait Observer {
    fn before(&mut self, url: &Url);
    fn after(&mut self, url: &Url, response: &Response<Body>);
}

/// The final response and how it was reached.
pub struct Outcome {
    pub url: Url,
    pub response: Response<Body>,
    pub redirects: u32,
}

/// An agent configured for one run.
pub struct Client {
    agent: Agent,
    tls_log: Arc<Mutex<TlsLog>>,
}

impl Client {
    pub fn new(opts: &Options, tls: Arc<ClientConfig>) -> Client {
        let tls_log = Arc::new(Mutex::new(TlsLog::default()));
        let connector = ()
            .chain(TcpConnector::default())
            .chain(TlsConnector::new(tls, opts.verbose, tls_log.clone()));
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            // No proxy from the environment: on LazyOS there is none, and a
            // proxy variable must not silently reroute verified traffic.
            .proxy(None)
            .user_agent(
                opts.user_agent
                    .clone()
                    .unwrap_or_else(|| USER_AGENT.to_string()),
            )
            .accept_encoding("gzip")
            .timeout_connect(opts.connect_timeout)
            // One request chain per process: a pooled connection would only
            // save a handshake on a same-host redirect, and reusing one the
            // server has already closed fails the next hop.
            .max_idle_connections(0)
            .max_idle_connections_per_host(0)
            .build();
        let agent = Agent::with_parts(config, connector, SyncResolver);
        Client { agent, tls_log }
    }

    /// Request `url` and follow redirects when `opts.follow`.
    pub fn fetch(
        &self,
        opts: &Options,
        url: Url,
        observer: &mut dyn Observer,
    ) -> Result<Outcome, Failure> {
        let deadline = opts.max_time.map(|limit| Instant::now() + limit);
        let mut url = url;
        let mut redirects = 0;
        loop {
            observer.before(&url);
            let response = self.send(opts, &url, remaining(deadline)?)?;
            observer.after(&url, &response);
            let status = response.status().as_u16();
            if !opts.follow || !redirect::is_redirect(status) {
                return Ok(Outcome {
                    url,
                    response,
                    redirects,
                });
            }
            let location = response.headers().get("location").map(|v| v.as_bytes());
            url = redirect::next_hop(&url, location, redirects, opts.max_redirs)?;
            redirects += 1;
        }
    }

    fn send(
        &self,
        opts: &Options,
        url: &Url,
        limit: Option<Duration>,
    ) -> Result<Response<Body>, Failure> {
        if let Ok(mut log) = self.tls_log.lock() {
            log.failure = None;
        }
        let target = redirect::request_target(url);
        let result = if opts.head {
            let mut req = self.agent.head(&target);
            for (name, value) in &opts.headers {
                req = req.header(name.as_str(), value.as_str());
            }
            req.config().timeout_global(limit).build().call()
        } else {
            let mut req = self.agent.get(&target);
            for (name, value) in &opts.headers {
                req = req.header(name.as_str(), value.as_str());
            }
            req.config().timeout_global(limit).build().call()
        };
        result.map_err(|e| self.failure(e, url))
    }

    /// Map a ureq error to a [`Failure`], using what the TLS connector
    /// recorded for handshake failures.
    fn failure(&self, error: ureq::Error, url: &Url) -> Failure {
        let host = url.host_str().unwrap_or("").to_string();
        match error {
            ureq::Error::HostNotFound => Failure::Resolve(host),
            ureq::Error::Timeout(_) => Failure::Timeout,
            ureq::Error::Tls(what) => {
                let recorded = self
                    .tls_log
                    .lock()
                    .ok()
                    .and_then(|mut log| log.failure.take());
                Failure::Tls(recorded.unwrap_or(TlsFailure {
                    reason: what.to_string(),
                    certificate: false,
                    clock: None,
                }))
            }
            ureq::Error::Io(e) => io_failure(&e, &host),
            ureq::Error::ConnectionFailed => Failure::Connect(host),
            ureq::Error::BadUri(u) => Failure::BadUrl(printable(u.as_bytes())),
            ureq::Error::Http(e) => Failure::Usage(e.to_string()),
            other => Failure::Recv(printable(other.to_string().as_bytes())),
        }
    }
}

/// Connection-stage I/O errors are "could not connect"; the rest are
/// failures while receiving.
fn io_failure(error: &io::Error, host: &str) -> Failure {
    use io::ErrorKind::*;
    match error.kind() {
        ConnectionRefused | HostUnreachable | NetworkUnreachable | AddrNotAvailable
        | NotConnected => Failure::Connect(format!("{host}: {error}")),
        TimedOut | WouldBlock => Failure::Timeout,
        _ => Failure::Recv(printable(error.to_string().as_bytes())),
    }
}

/// The time left before `deadline`, or a timeout when none is.
fn remaining(deadline: Option<Instant>) -> Result<Option<Duration>, Failure> {
    let Some(deadline) = deadline else {
        return Ok(None);
    };
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Failure::Timeout);
    }
    Ok(Some(left))
}

/// `HTTP/1.1 200 OK`, with the reason from the status code (HTTP/1.1
/// reason phrases are not kept by ureq, and carry no meaning).
pub fn status_line(response: &Response<Body>) -> String {
    let status = response.status();
    format!(
        "{:?} {} {}",
        response.version(),
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    )
    .trim_end()
    .to_string()
}

/// The response headers as `name: value` lines, printable ASCII only.
pub fn header_lines(response: &Response<Body>) -> Vec<String> {
    response
        .headers()
        .iter()
        .map(|(name, value)| format!("{}: {}", name.as_str(), printable(value.as_bytes())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_expiry_is_a_timeout() {
        assert_eq!(remaining(None).unwrap(), None);
        let past = Instant::now() - Duration::from_millis(1);
        assert_eq!(remaining(Some(past)), Err(Failure::Timeout));
        let soon = Instant::now() + Duration::from_secs(60);
        assert!(remaining(Some(soon)).unwrap().unwrap() > Duration::from_secs(50));
    }

    #[test]
    fn io_errors_split_connect_from_receive() {
        let refused = io::Error::from(io::ErrorKind::ConnectionRefused);
        assert!(matches!(io_failure(&refused, "h"), Failure::Connect(_)));
        let reset = io::Error::from(io::ErrorKind::ConnectionReset);
        assert!(matches!(io_failure(&reset, "h"), Failure::Recv(_)));
    }
}
