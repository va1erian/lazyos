//! What the address bar turns typed text into.
//!
//! A URL with a scheme is taken as is; an absolute path is a `file:` URL;
//! anything else is a host (and maybe a path) reached over plain HTTP, the
//! way a browser of the 90s would: `example.com` opens `http://example.com/`.
//! The built-in start page has the short name [`START`].

/// The start page's name in the address bar.
pub const START: &str = "about:start";

/// Schemes taken as typed, without `//` (the others need `scheme://`).
const OPAQUE_SCHEMES: &[&str] = &["about:", "data:", "javascript:", "mailto:"];

/// The URL to open for the address bar's `text`, or `None` when there is
/// nothing to open.
pub fn normalize(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    // `://` is a scheme only before the path, query or fragment starts:
    // `example.com/go?to=https://x.org` is still a bare host.
    let has_scheme = text
        .find("://")
        .is_some_and(|at| !text[..at].contains(['/', '?', '#']));
    if has_scheme || has_prefix(text, OPAQUE_SCHEMES) {
        return Some(text.to_string());
    }
    if text.starts_with('/') {
        return Some(format!("file://{text}"));
    }
    // A bare host: an address with no path gets `/`, as a server expects.
    let has_path = text.contains(['/', '?', '#']);
    let slash = if has_path { "" } else { "/" };
    Some(format!("http://{text}{slash}"))
}

/// Whether `url` is fetched over the network (`http:` or `https:`).
pub fn is_network(url: &str) -> bool {
    has_prefix(url, &["http://", "https://"])
}

fn has_prefix(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| {
        text.get(..p.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(p))
    })
}

/// `data:` URL of an HTML page, percent-encoded (NetSurf reads `data:`
/// itself, so the start page needs no file on disk).
pub fn html_data_url(html: &str) -> String {
    let mut url = String::from("data:text/html;charset=utf-8,");
    for byte in html.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~ /:=<>\"'!".contains(&byte) {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_hosts_become_http_urls() {
        assert_eq!(normalize("example.com").unwrap(), "http://example.com/");
        assert_eq!(
            normalize("  example.com/a b ").unwrap(),
            "http://example.com/a b"
        );
        assert_eq!(
            normalize("localhost:8080").unwrap(),
            "http://localhost:8080/"
        );
        assert_eq!(
            normalize("10.0.2.2:8080?x").unwrap(),
            "http://10.0.2.2:8080?x"
        );
        assert_eq!(
            normalize("example.com/go?to=https://x.org").unwrap(),
            "http://example.com/go?to=https://x.org"
        );
    }

    #[test]
    fn urls_with_a_scheme_are_kept() {
        for url in [
            "https://theoldnet.com/",
            "HTTP://EXAMPLE.COM/",
            "file:///docs/os/index.html",
            "about:blank",
            "data:text/html,<p>x</p>",
        ] {
            assert_eq!(normalize(url).unwrap(), url);
        }
    }

    #[test]
    fn paths_become_file_urls() {
        assert_eq!(
            normalize("/tmp/page.html").unwrap(),
            "file:///tmp/page.html"
        );
    }

    #[test]
    fn network_urls() {
        assert!(is_network("HTTPS://x/") && is_network("http://x/"));
        assert!(!is_network("file:///x") && !is_network("data:,x") && !is_network("http"));
    }

    #[test]
    fn nothing_to_open() {
        assert_eq!(normalize("   "), None);
        assert_eq!(normalize("a\u{7}b"), None);
    }

    #[test]
    fn data_urls_escape_what_a_url_cannot_hold() {
        let url = html_data_url("<a href=\"x#y\">100%</a>\n");
        assert_eq!(
            url,
            "data:text/html;charset=utf-8,<a href=\"x%23y\">100%25</a>%0A"
        );
    }
}
