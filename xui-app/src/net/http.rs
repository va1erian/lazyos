//! The little HTTP/1.0 Net Tools speaks, as pure functions: parsing a typed
//! URL, the request it sends, a summary of what came back, and the page its
//! web server answers with.
//!
//! Everything a peer sends is untrusted: lines are clipped, control characters
//! dropped, and nothing is parsed beyond the status line and the header/body
//! split. There is no TLS on LazyOS yet, so `https://` is refused up front.

use crate::format;

/// A parsed `http://host[:port][/path]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
}

/// The longest host name DNS allows.
const MAX_HOST: usize = 253;
/// The longest path sent.
const MAX_PATH: usize = 1024;
/// Body lines a fetch summary keeps.
const PREVIEW_LINES: usize = 6;
/// Characters per preview or log line.
const LINE_CHARS: usize = 96;

/// Parse what the user typed; the scheme may be left out.
pub fn parse_url(text: &str) -> Result<Url, String> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Err(String::from(
            "https needs TLS, which LazyOS does not have yet: use http://",
        ));
    }
    let rest = if lower.starts_with("http://") {
        &text[7..]
    } else {
        text
    };
    if rest.contains("://") {
        return Err(String::from("only http:// URLs are supported"));
    }
    let (authority, path) = match rest.find('/') {
        Some(slash) => (&rest[..slash], &rest[slash..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| format!("bad port {port:?}"))?;
            (host, port)
        }
        None => (authority, 80),
    };
    if !valid_host(host) {
        return Err(format!("bad host name {host:?}"));
    }
    if path.len() > MAX_PATH || !path.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(String::from(
            "the path must be printable ASCII without spaces",
        ));
    }
    Ok(Url {
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

/// A host name or dotted quad: letters, digits, `-` and `.`, labels of 1 to 63.
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_HOST
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// The request for `url` (HTTP/1.0, so the server closes when done).
pub fn request(url: &Url) -> String {
    let host = if url.port == 80 {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    format!(
        "GET {} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: LazyOS-NetTools/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        url.path
    )
}

/// What a fetch got back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// The status line (`HTTP/1.1 200 OK`), or a note that there was none.
    pub status: String,
    /// The numeric status, when the line had one.
    pub code: Option<u16>,
    pub headers: usize,
    pub body_bytes: usize,
    /// The first non-empty body lines, cleaned and clipped.
    pub preview: Vec<String>,
}

/// Summarise a complete response.
pub fn summarize(response: &[u8]) -> Summary {
    let split = find(response, b"\r\n\r\n")
        .map(|at| (at, 4))
        .or_else(|| find(response, b"\n\n").map(|at| (at, 2)));
    let (head, body) = match split {
        Some((at, sep)) => (&response[..at], &response[at + sep..]),
        None => (response, &response[response.len()..]),
    };
    let head = String::from_utf8_lossy(head);
    let mut lines = head.lines();
    let status = lines.next().map(clean_line).unwrap_or_default();
    let code = status
        .starts_with("HTTP/")
        .then(|| status.split_whitespace().nth(1)?.parse().ok())
        .flatten();
    let preview = String::from_utf8_lossy(body)
        .lines()
        .map(clean_line)
        .filter(|line| !line.trim().is_empty())
        .take(PREVIEW_LINES)
        .collect();
    Summary {
        status: if status.is_empty() {
            String::from("(no status line)")
        } else {
            status
        },
        code,
        headers: lines.filter(|line| !line.trim().is_empty()).count(),
        body_bytes: body.len(),
        preview,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A peer's line made safe for a label: no control characters, clipped.
pub fn clean_line(line: &str) -> String {
    let text: String = line.chars().filter(|c| !c.is_control()).collect();
    format::clip(text.trim_end(), LINE_CHARS)
}

/// The first line of a request (`GET / HTTP/1.1`), for the server's log.
pub fn request_line(request: &[u8]) -> String {
    let end = request
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or(request.len());
    let line = clean_line(&String::from_utf8_lossy(&request[..end]));
    if line.is_empty() {
        String::from("(empty request)")
    } else {
        line
    }
}

/// Whether `request` holds a complete request head.
pub fn head_complete(request: &[u8]) -> bool {
    find(request, b"\r\n\r\n").is_some() || find(request, b"\n\n").is_some()
}

/// What the served page shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageInfo {
    /// The stack's headline (`eth0 10.0.2.15/24 via 10.0.2.2 · link up`).
    pub network: String,
    /// Uptime as `H:MM:SS`.
    pub uptime: String,
}

/// The full response the web server sends: a small HTML page about this
/// machine and the visitor. `visitor` is the peer's `address:port`.
pub fn page(info: &PageInfo, visitor: &str, hits: u64) -> Vec<u8> {
    let body = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>LazyOS</title>\n\
         <style>body{{font-family:sans-serif;max-width:40em;margin:3em auto;color:#222}}\
         code{{background:#eee;padding:0 .3em}}</style></head>\n<body>\n\
         <h1>Hello from LazyOS</h1>\n\
         <p>This page is served by <b>Net Tools</b>, a desktop app running inside the \
         QEMU machine, over LazyOS's own TCP/IP stack (<code>netd</code>).</p>\n\
         <ul>\n<li>Network: {}</li>\n<li>Up: {}</li>\n<li>You are {} (QEMU's user network \
         shows the host as 10.0.2.2)</li>\n<li>Requests served: {}</li>\n</ul>\n\
         </body></html>\n",
        escape(&info.network),
        escape(&info.uptime),
        escape(visitor),
        hits
    );
    let mut response = format!(
        "HTTP/1.0 200 OK\r\nServer: LazyOS-NetTools/0.1\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body.as_bytes());
    response
}

/// `text` safe inside HTML.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_parse_with_or_without_a_scheme() {
        let url = parse_url("http://example.com").unwrap();
        assert_eq!(
            (url.host.as_str(), url.port, url.path.as_str()),
            ("example.com", 80, "/")
        );
        let url = parse_url(" 10.0.2.2:8000/a/b?c=d ").unwrap();
        assert_eq!(
            (url.host.as_str(), url.port, url.path.as_str()),
            ("10.0.2.2", 8000, "/a/b?c=d")
        );
        assert_eq!(
            parse_url("HTTP://Example.com/x").unwrap().host,
            "Example.com"
        );
        assert_eq!(request(&url).lines().nth(1), Some("Host: 10.0.2.2:8000"));
        assert!(request(&parse_url("a.b").unwrap()).starts_with("GET / HTTP/1.0\r\nHost: a.b\r\n"));
    }

    #[test]
    fn bad_urls_are_refused_with_a_reason() {
        assert!(parse_url("https://example.com")
            .unwrap_err()
            .contains("TLS"));
        for bad in [
            "",
            "ftp://x",
            "http://",
            "host:0",
            "host:99999",
            "ho st",
            "a..b",
            "http://x/sp ace",
            "-bad_host_",
            &format!("{}.com", "a".repeat(64)),
        ] {
            assert!(parse_url(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn responses_are_summarised_and_cleaned() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nX: y\r\n\r\n<html>\n\n<h1>Hi\x1b[2J</h1>\n";
        let s = summarize(response);
        assert_eq!(s.status, "HTTP/1.1 200 OK");
        assert_eq!(s.code, Some(200));
        assert_eq!(s.headers, 2);
        assert_eq!(s.body_bytes, b"<html>\n\n<h1>Hi\x1b[2J</h1>\n".len());
        assert_eq!(s.preview, vec!["<html>", "<h1>Hi[2J</h1>"]);
        let s = summarize(b"garbage without a head");
        assert_eq!((s.code, s.body_bytes), (None, 0));
        assert_eq!(summarize(b"").status, "(no status line)");
        let long = format!("HTTP/1.0 404 {}\n\n", "x".repeat(500));
        let s = summarize(long.as_bytes());
        assert_eq!(s.code, Some(404));
        assert_eq!(s.status.chars().count(), LINE_CHARS);
    }

    #[test]
    fn requests_are_logged_safely() {
        assert_eq!(
            request_line(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"),
            "GET / HTTP/1.1"
        );
        assert_eq!(request_line(b""), "(empty request)");
        assert!(head_complete(b"GET / HTTP/1.0\r\n\r\n"));
        assert!(!head_complete(b"GET / HTTP/1.0\r\n"));
    }

    #[test]
    fn the_page_is_a_complete_response_and_escapes_what_it_shows() {
        let info = PageInfo {
            network: "eth0 <b>".into(),
            uptime: "0:42".into(),
        };
        let response = page(&info, "10.0.2.2:5555", 3);
        let text = String::from_utf8(response).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.0 200 OK"));
        assert!(head.contains(&format!("Content-Length: {}", body.len())));
        assert!(body.contains("eth0 &lt;b&gt;"));
        assert!(body.contains("Requests served: 3"));
        assert!(body.contains("10.0.2.2:5555"));
    }
}
