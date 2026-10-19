//! A blocking HTTP/1.1 client with caps (docs/lazysky-plan.md §7.2), an
//! allowlist [`Policy`] (docs/ai-agent-plan.md §3) and a streaming path whose
//! reads wake every few hundred milliseconds to look at a stop flag
//! (docs/ai-agent-plan.md §5.2).
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

use crate::resolve::InlineResolver;
pub use crate::tls::Watch;
use crate::tls::{self, LazyConfig, TlsConnector};
use crate::Roots;

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

/// Starts the message of a request the [`Policy`] refused, so a caller can
/// tell a refusal from a network failure.
pub const REFUSED: &str = "refused: ";

/// Which URLs a client may request. The default is LazySky's: any host,
/// `http` or `https`. LazyAgent's is [`Policy::https_hosts`].
#[derive(Clone, Debug, Default)]
pub struct Policy {
    /// `None` allows every host. An entry is a bare host (port 443 only) or
    /// `host:port` (that port only).
    hosts: Option<Vec<String>>,
}

impl Policy {
    /// `https` only, to the listed hosts only.
    pub fn https_hosts(hosts: &[&str]) -> Policy {
        Policy {
            hosts: Some(hosts.iter().map(|h| h.to_ascii_lowercase()).collect()),
        }
    }

    /// Whether `url` may be requested. A refusal says why, in words.
    pub fn check(&self, url: &str) -> Result<(), String> {
        let uri: http::Uri = url
            .parse()
            .map_err(|_| format!("not a usable URL: {url}"))?;
        let scheme = uri.scheme_str().unwrap_or("").to_ascii_lowercase();
        let Some(allowed) = &self.hosts else {
            return match scheme.as_str() {
                "http" | "https" => Ok(()),
                _ => Err(format!("cannot fetch {url} (only http and https)")),
            };
        };
        let authority = uri
            .authority()
            .ok_or_else(|| format!("not a usable URL: {url}"))?;
        let shown = authority.as_str().rsplit('@').next().unwrap_or("");
        if scheme != "https" {
            return Err(format!("{REFUSED}{shown}: only https is allowed"));
        }
        if authority.as_str().contains('@') {
            return Err(format!(
                "{REFUSED}{shown}: a URL with a user name is refused"
            ));
        }
        let host = authority.host().to_ascii_lowercase();
        let port = authority.port_u16().unwrap_or(443);
        let ok = allowed.iter().any(|entry| match entry.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
                h == host && p.parse() == Ok(port)
            }
            _ => *entry == host && port == 443,
        });
        if ok {
            Ok(())
        } else {
            Err(format!(
                "{REFUSED}{shown}: this host is not on the allowed list"
            ))
        }
    }
}

#[derive(Clone, Debug)]
pub struct HttpOptions {
    pub user_agent: String,
    /// Which URLs may be requested at all.
    pub policy: Policy,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    /// A body longer than this is an error, not a truncation.
    pub max_body: usize,
}

impl Default for HttpOptions {
    fn default() -> HttpOptions {
        HttpOptions {
            user_agent: concat!("lazyhttps/", env!("CARGO_PKG_VERSION")).to_string(),
            policy: Policy::default(),
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

/// A response whose body is still on the wire. Reading it wakes the
/// connection's [`Watch`] (so Stop and a stalled server are noticed) and
/// ends with an `Interrupted` error once the stop flag is set, or a
/// `TimedOut` one after the idle limit.
pub struct StreamResponse {
    pub status: u16,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    reader: ureq::BodyReader<'static>,
    watch: Watch,
}

impl StreamResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

impl Read for StreamResponse {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let _watching = tls::watching(self.watch.clone());
        self.reader.read(buf)
    }
}

pub struct Http {
    agent: Agent,
    /// The same connector with no body or response timeout: the [`Watch`]
    /// bounds a stream instead.
    stream_agent: Agent,
    options: HttpOptions,
    /// Keeps `Http` on the thread that made it.
    _thread: PhantomData<*const ()>,
}

impl Http {
    pub fn new(roots: Roots, options: HttpOptions) -> Http {
        let tls = Arc::new(LazyConfig::new(roots));
        // Both agents share one lazily made TLS configuration.
        let connector = || ().chain(TcpConnector::default()).chain(TlsConnector::new(tls.clone()));
        let config = |recv: Option<Duration>| {
            Agent::config_builder()
                .http_status_as_error(false)
                .max_redirects(0)
                // Never from the environment: a stray variable must not
                // reroute the client's traffic.
                .proxy(None)
                .user_agent(AutoHeaderValue::None)
                .accept_encoding(AutoHeaderValue::None)
                .timeout_connect(Some(options.connect_timeout))
                .timeout_send_request(Some(options.read_timeout))
                .timeout_send_body(Some(options.read_timeout))
                .timeout_recv_response(recv)
                .timeout_recv_body(recv)
                .max_idle_connections(0)
                .max_idle_connections_per_host(0)
                .build()
        };
        let stream_agent = Agent::with_parts(config(None), connector(), InlineResolver);
        Http {
            agent: Agent::with_parts(
                config(Some(options.read_timeout)),
                connector(),
                InlineResolver,
            ),
            stream_agent,
            options,
            _thread: PhantomData,
        }
    }

    /// `GET url` with extra `headers`.
    pub fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Response, String> {
        self.send(http::Method::GET, url, headers, None)
    }

    /// `GET url`, following up to `hops` redirects to another `https`
    /// address. For pictures and other public files only: the redirect
    /// target is a different server, so no header of the caller's is sent
    /// there except the ones in `headers`, and the API client (which sends a
    /// bearer token) never uses this.
    pub fn get_following(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        hops: usize,
    ) -> Result<Response, String> {
        let mut current = url.to_string();
        for _ in 0..=hops {
            let response = self.get(&current, headers)?;
            let Some(next) = response
                .header("location")
                .and_then(|location| redirect_target(&current, response.status, location))
            else {
                return Ok(response);
            };
            current = next;
        }
        Err(format!("{}: too many redirects", host_of(url)))
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

    /// `POST url` and returns as soon as the response head is in; the body
    /// is read from the [`StreamResponse`]. Waiting for the head and every
    /// later read wake every `watch.poll` and give up when `watch.stop` is
    /// set or `watch.idle` passes with no byte.
    pub fn post_stream(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &[u8],
        watch: Watch,
    ) -> Result<StreamResponse, String> {
        let response = {
            let _watching = tls::watching(watch.clone());
            self.run(
                &self.stream_agent,
                http::Method::POST,
                url,
                headers,
                Some(body),
            )?
        };
        let (parts, body) = response.into_parts();
        Ok(StreamResponse {
            status: parts.status.as_u16(),
            headers: lowercased(&parts.headers),
            reader: body.into_reader(),
            watch,
        })
    }

    fn send(
        &self,
        method: http::Method,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<Response, String> {
        let host = host_of(url);
        let response = self.run(&self.agent, method, url, headers, body)?;
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
        let headers = lowercased(&parts.headers);
        Ok(Response {
            status: parts.status.as_u16(),
            headers,
            body: bytes,
        })
    }
}

impl Http {
    /// Checks the policy, builds the request and sends it; the response
    /// head comes back with its body unread.
    fn run(
        &self,
        agent: &Agent,
        method: http::Method,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<http::Response<ureq::Body>, String> {
        let host = host_of(url);
        self.options.policy.check(url)?;
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
            Some(bytes) => agent.run(builder.body(bytes).map_err(bad)?),
            None => agent.run(builder.body(()).map_err(bad)?),
        };
        result.map_err(|e| describe(e, &host))
    }
}

fn lowercased(headers: &http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(n, v)| Some((n.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect()
}

/// Where a redirect answer points, when it may be followed: a 301, 302,
/// 303, 307 or 308 whose `location` is an `https` address or a path on the
/// same server. Never a downgrade to `http` and never another scheme.
fn redirect_target(from: &str, status: u16, location: &str) -> Option<String> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let location = location.trim();
    let secure = |u: &str| {
        u.get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
    };
    if secure(location) {
        return Some(location.to_string());
    }
    if location.starts_with('/') && !location.starts_with("//") && secure(from) {
        let scheme_end = from.find("://")? + 3;
        let host_end = from[scheme_end..]
            .find(['/', '?', '#'])
            .map_or(from.len(), |i| scheme_end + i);
        return Some(format!("{}{location}", &from[..host_end]));
    }
    None
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
    fn redirects_are_followed_only_to_https_or_a_path_on_the_same_server() {
        let from = "https://video.bsky.app/watch/x/thumbnail.jpg?a=1";
        let go = |status, location| redirect_target(from, status, location);
        assert_eq!(
            go(302, "https://cdn.example/t.jpg").as_deref(),
            Some("https://cdn.example/t.jpg")
        );
        assert_eq!(
            go(301, "/other/t.jpg").as_deref(),
            Some("https://video.bsky.app/other/t.jpg")
        );
        assert_eq!(go(200, "https://cdn.example/t.jpg"), None, "not a redirect");
        assert_eq!(go(302, "http://cdn.example/t.jpg"), None, "no downgrade");
        assert_eq!(go(302, "//evil.example/t.jpg"), None, "scheme-relative");
        assert_eq!(go(302, "ftp://evil.example/t"), None);
        assert_eq!(go(304, "/x"), None);
    }

    #[test]
    fn other_schemes_are_refused_before_the_network() {
        let http = Http::new(Roots::Pem(Arc::default()), HttpOptions::default());
        let err = http.get("ftp://example.org/x", &[]).unwrap_err();
        assert!(err.contains("only http and https"), "{err}");
    }
}
