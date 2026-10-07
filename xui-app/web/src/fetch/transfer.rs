//! One fetch, start to end, on the calling thread: send the request, report
//! the status and headers, stream the decoded body.

use std::io::{self, Read};
use std::sync::{Arc, Mutex};

use ureq::config::AutoHeaderValue;
use ureq::http::{self, HeaderMap, Response};
use ureq::unversioned::transport::{Connector, TcpConnector};
use ureq::{Agent, Body, Error};

use super::cookies::{self, Jar};
use super::decode::{self, Coding};
use super::resolve::InlineResolver;
use super::tls::{self, LazyConfig, TlsConnector};
use super::trace::Timing;
use super::{Method, Options, Request, Sink};

/// The size of the chunks handed to the sink.
const CHUNK: usize = 16 * 1024;

/// What we accept: the two codings [`decode`] handles.
const ACCEPT_ENCODING: &str = "gzip, deflate";

/// Request headers the fetcher owns: connection management, framing and
/// content coding are ours to decide, whatever the view passes.
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

/// The agent every fetch shares: our resolver and TLS, no redirects, no
/// proxy, no pooled connections (a LazyOS thread's sockets are its own).
pub(crate) fn agent(options: &Options) -> Agent {
    let tls = Arc::new(LazyConfig::new(options.roots.clone()));
    let connector = ().chain(TcpConnector::default()).chain(TlsConnector::new(tls));
    let config = Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        // Never from the environment: a stray variable must not reroute the
        // browser's traffic.
        .proxy(None)
        .user_agent(AutoHeaderValue::None)
        .accept_encoding(AutoHeaderValue::None)
        .timeout_connect(Some(options.connect_timeout))
        .timeout_send_request(Some(options.response_timeout))
        .timeout_send_body(Some(options.response_timeout))
        .timeout_recv_response(Some(options.response_timeout))
        .timeout_recv_body(Some(options.body_timeout))
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .build();
    Agent::with_parts(config, connector, InlineResolver)
}

/// Runs `request` and reports it to `sink`, then its `WEB:FETCH` line.
pub(crate) fn run<S: Sink>(
    agent: &Agent,
    options: &Options,
    jar: &Mutex<Jar>,
    request: Request,
    sink: S,
) {
    let url = request.url.clone();
    let mut timing = Timing::start();
    let outcome = exchange(agent, options, jar, request, sink, &mut timing);
    timing.report(&outcome, &url);
}

/// Does the fetch; its status, or `FAIL`.
fn exchange<S: Sink>(
    agent: &Agent,
    options: &Options,
    jar: &Mutex<Jar>,
    mut request: Request,
    sink: S,
    timing: &mut Timing,
) -> String {
    let failed = |sink: S, why: &str| {
        sink.fail(why);
        "FAIL".to_string()
    };
    if sink.is_aborted() {
        return failed(sink, "aborted");
    }
    let host = host_of(&request.url);
    let head = request.method == Method::Head;
    let url = request.url.clone();
    send_cookies(jar, &mut request);
    let response = match send(agent, options, request) {
        Ok(response) => response,
        Err(why) => return failed(sink, &why.describe(&host)),
    };
    timing.headers();
    let (parts, body) = response.into_parts();
    let status = parts.status.as_u16();
    let coding = Coding::of(header_str(&parts.headers, "content-encoding").as_deref());
    sink.status(status);
    keep_cookies(jar, &url, &parts.headers);
    report_headers(&sink, &parts.headers, coding);
    if !head && has_body(status) {
        if let Err(why) = stream(&sink, body, coding, options.max_body, timing) {
            return failed(sink, &why);
        }
    }
    sink.finish();
    status.to_string()
}

/// Why a request got no response.
enum Failure {
    Url(String),
    Net(Error),
}

impl Failure {
    fn describe(self, host: &str) -> String {
        let error = match self {
            Failure::Url(why) => return why,
            Failure::Net(error) => error,
        };
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
}

fn send(agent: &Agent, options: &Options, request: Request) -> Result<Response<Body>, Failure> {
    let scheme_ok = ["http://", "https://"].iter().any(|s| {
        request
            .url
            .get(..s.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(s))
    });
    if !scheme_ok {
        return Err(Failure::Url(format!(
            "cannot fetch {} (only http and https)",
            request.url
        )));
    }
    let method = match request.method {
        Method::Get => http::Method::GET,
        Method::Head => http::Method::HEAD,
        Method::Post => http::Method::POST,
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(request.url.as_str());
    let mut has_agent = false;
    for (name, value) in &request.headers {
        let lower = name.to_ascii_lowercase();
        if OWN_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        has_agent |= lower == "user-agent";
        builder = builder.header(name.as_str(), value.as_str());
    }
    if !has_agent {
        builder = builder.header("user-agent", options.user_agent.as_str());
    }
    builder = builder.header("accept-encoding", ACCEPT_ENCODING);
    let bad =
        |e: http::Error| Failure::Url(format!("not a usable request for {}: {e}", request.url));
    let result = match request.body {
        Some(body) => agent.run(builder.body(body).map_err(bad)?),
        None => agent.run(builder.body(()).map_err(bad)?),
    };
    result.map_err(Failure::Net)
}

/// Adds the jar's `Cookie` header, unless the request names its own.
fn send_cookies(jar: &Mutex<Jar>, request: &mut Request) {
    if request.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("cookie")) {
        return;
    }
    let header = jar
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .header(&request.url, cookies::now());
    if let Some(value) = header {
        request.headers.push(("Cookie".to_string(), value));
    }
}

/// Puts the response's `Set-Cookie` headers in the jar.
fn keep_cookies(jar: &Mutex<Jar>, url: &str, headers: &HeaderMap) {
    let now = cookies::now();
    let mut jar = jar.lock().unwrap_or_else(|p| p.into_inner());
    for value in headers.get_all("set-cookie") {
        jar.store(url, &String::from_utf8_lossy(value.as_bytes()), now);
    }
}

/// Passes the response headers on, minus framing and the coding we undo.
fn report_headers<S: Sink>(sink: &S, headers: &HeaderMap, coding: Coding) {
    for (name, value) in headers {
        let name = name.as_str();
        let dropped = match name {
            "transfer-encoding" | "connection" | "keep-alive" => true,
            "content-encoding" | "content-length" => coding.decoded(),
            _ => false,
        };
        if !dropped {
            sink.header(name, &String::from_utf8_lossy(value.as_bytes()));
        }
    }
}

/// Whether a response with `status` can carry a body (RFC 9110 §6.4.1).
fn has_body(status: u16) -> bool {
    !(100..200).contains(&status) && status != 204 && status != 304
}

/// Streams the body to `sink`, decoded, stopping at the cap or an abort.
fn stream<S: Sink>(
    sink: &S,
    body: Body,
    coding: Coding,
    max: u64,
    timing: &mut Timing,
) -> Result<(), String> {
    // The raw body is capped too, so a slow trickle of compressed bytes that
    // never inflates past the cap still cannot run forever (ureq's body
    // timeout also applies).
    let raw = body.into_with_config().limit(max).reader();
    let mut reader = decode::reader(raw, coding).map_err(|e| read_error(&e, max))?;
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        if sink.is_aborted() {
            return Err("aborted".into());
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(read_error(&e, max)),
        };
        total += n as u64;
        timing.add(n);
        if total > max {
            return Err(too_large(max));
        }
        sink.data(&buf[..n]);
    }
}

fn too_large(max: u64) -> String {
    format!("the page is larger than {} MiB", max / (1024 * 1024))
}

fn read_error(error: &io::Error, max: u64) -> String {
    let inner = error.get_ref().and_then(|e| e.downcast_ref::<Error>());
    match inner {
        Some(Error::BodyExceedsLimit(_)) => too_large(max),
        Some(Error::Timeout(_)) => "the server stopped sending".into(),
        Some(other) => format!("the transfer broke off: {other}"),
        None if error.kind() == io::ErrorKind::InvalidInput
            || error.kind() == io::ErrorKind::InvalidData =>
        {
            format!("the page could not be decoded: {error}")
        }
        None => format!("the transfer broke off: {error}"),
    }
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
}

/// The URL's host (with port), for messages.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.rsplit('@').next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_for_messages() {
        assert_eq!(host_of("http://example.com/a?b"), "example.com");
        assert_eq!(host_of("https://u:p@h:8443#x"), "h:8443");
    }

    #[test]
    fn bodiless_statuses() {
        assert!(!has_body(101) && !has_body(204) && !has_body(304));
        assert!(has_body(200) && has_body(302) && has_body(404));
    }
}
