//! What the address bar turns typed text into.
//!
//! A URL with a scheme is taken as is; an absolute path is a `file:` URL; text that looks like a
//! host (`example.com`, `localhost:8080`, an IP literal, `intranet/wiki`) is opened over HTTPS
//! first (see [`crate::downgrade`] for the way back to HTTP); anything else, or text that starts
//! with `?`, is a web search through the person's template ([`DEFAULT_SEARCH`]).
//! The built-in start page has the short name [`START`].

/// The start page's name in the address bar.
pub const START: &str = "about:start";

/// The search template used until the person sets their own (`prefs.rs`); `%s` is the query.
pub const DEFAULT_SEARCH: &str = "https://duckduckgo.com/?q=%s";

/// The longest search template taken.
const MAX_TEMPLATE: usize = 512;

/// Schemes taken as typed, without `//` (the others need `scheme://`).
const OPAQUE_SCHEMES: &[&str] = &["about:", "data:", "javascript:", "mailto:"];

/// What typed text means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// An address to open as it is (it has a scheme, or is a file).
    Url(String),
    /// A host, with its port and path: `https` first, `http` if the person allows it.
    Host {
        /// `host[:port]` in lower case: what an allowed downgrade is remembered by.
        host: String,
        https: String,
        http: String,
    },
    /// A search: the URL of the results.
    Search(String),
}

impl Typed {
    /// The URL to open first.
    pub fn url(&self) -> &str {
        match self {
            Typed::Url(url) | Typed::Search(url) => url,
            Typed::Host { https, .. } => https,
        }
    }
}

/// What to open for the address bar's `text`, with the default search template, or `None` when there
/// is nothing to open.
pub fn normalize(text: &str) -> Option<String> {
    classify(text, DEFAULT_SEARCH).map(|typed| typed.url().to_owned())
}

/// Classifies the address bar's `text`; `search` is the search template (an invalid one is replaced
/// by the default).
pub fn classify(text: &str, search: &str) -> Option<Typed> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    // `?query` is a search whatever it holds (`?example.com` searches for the word).
    if let Some(query) = text.strip_prefix('?') {
        let query = query.trim();
        return (!query.is_empty()).then(|| Typed::Search(search_url(search, query)));
    }
    // `://` is a scheme only before the path, query or fragment starts:
    // `example.com/go?to=https://x.org` is still a bare host.
    let has_scheme = text
        .find("://")
        .is_some_and(|at| !text[..at].contains(['/', '?', '#']));
    if has_scheme || has_prefix(text, OPAQUE_SCHEMES) {
        return Some(Typed::Url(text.to_string()));
    }
    if text.starts_with('/') {
        return Some(Typed::Url(format!("file://{text}")));
    }
    match host_of(text) {
        Some(host) => {
            // A host with no path gets `/`, as a server expects.
            let slash = if text.contains(['/', '?', '#']) {
                ""
            } else {
                "/"
            };
            Some(Typed::Host {
                host: host.to_ascii_lowercase(),
                https: format!("https://{text}{slash}"),
                http: format!("http://{text}{slash}"),
            })
        }
        None => Some(Typed::Search(search_url(search, text))),
    }
}

/// The `host[:port]` of `text` when it reads as an address rather than words to search for.
pub(crate) fn host_of(text: &str) -> Option<&str> {
    let end = text.find(['/', '?', '#']).unwrap_or(text.len());
    let (authority, has_path) = (&text[..end], text[end..].starts_with('/'));
    if authority.is_empty() || authority.contains(char::is_whitespace) {
        return None;
    }
    let (name, port) = split_port(authority)?;
    if port.is_some_and(|p| p.is_empty() || p.len() > 5 || p.parse::<u16>().is_err()) {
        return None;
    }
    let is_host = if name.starts_with('[') {
        is_ipv6_literal(name)
    } else {
        is_ipv4(name)
            || name.eq_ignore_ascii_case("localhost")
            || is_domain(name)
            // `intranet/wiki`: one word is a host only when a path follows it.
            || (has_path && port.is_none() && is_label(name))
    };
    is_host.then_some(authority)
}

/// Whether `text` is a `host[:port]` a pending HTTPS offer can hold (what [`classify`] puts in
/// `Typed::Host::host`): unlike [`host_of`] it takes a bare label (`intranet` from `intranet/wiki`),
/// and it takes no path, whitespace, user part or control character.
pub(crate) fn is_offer_host(text: &str) -> bool {
    if text.is_empty()
        || text.contains(['/', '?', '#', '@'])
        || text.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    // The slash stands for the path that made a bare label a host when it was typed.
    let probe = format!("{text}/");
    host_of(&probe) == Some(text)
}

/// `name` and `port` of `authority` (`host:port`); an IPv6 literal keeps its colons.
fn split_port(authority: &str) -> Option<(&str, Option<&str>)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let close = rest.find(']')?;
        let name = &authority[..close + 2];
        return match &rest[close + 1..] {
            "" => Some((name, None)),
            tail => Some((name, Some(tail.strip_prefix(':')?))),
        };
    }
    match authority.split_once(':') {
        Some((name, port)) => (!port.contains(':')).then_some((name, Some(port))),
        None => Some((authority, None)),
    }
}

fn is_ipv6_literal(name: &str) -> bool {
    let inner = &name[1..name.len() - 1];
    inner.contains(':')
        && inner
            .chars()
            .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'))
}

fn is_ipv4(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        })
}

/// One label of a domain name: letters, digits and hyphens (any script), not starting or ending
/// with a hyphen.
fn is_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.chars().all(|c| c.is_alphanumeric() || c == '-')
}

/// A dotted name of two labels or more whose last label is a name, not a number (`1.5` is not a
/// host, `e.g.` has an empty label).
fn is_domain(name: &str) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name);
    let labels: Vec<&str> = name.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| is_label(l))
        && labels
            .last()
            .is_some_and(|tld| tld.chars().count() >= 2 && tld.chars().any(|c| !c.is_ascii_digit()))
}

/// Whether `template` can be used to search: an http(s) URL with `%s` for the query, no blanks.
pub fn valid_template(template: &str) -> bool {
    let lower = template.to_ascii_lowercase();
    template.len() <= MAX_TEMPLATE
        && template.contains("%s")
        && (lower.starts_with("http://") || lower.starts_with("https://"))
        && !template
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
}

/// The results URL for `query`: `%s` in `template` replaced by the percent-encoded query.
pub fn search_url(template: &str, query: &str) -> String {
    let template = if valid_template(template) {
        template
    } else {
        DEFAULT_SEARCH
    };
    template.replace("%s", &encode_query(query))
}

/// `text` percent-encoded for one query value: only unreserved characters stay, a space is `%20`.
pub fn encode_query(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Whether `url` is fetched over the network (`http:` or `https:`).
pub fn is_network(url: &str) -> bool {
    has_prefix(url, &["http://", "https://"])
}

/// The scheme of `url` in lower case (`mailto` for `MAILTO:a@b`), or `None` when it has none.
pub fn scheme(url: &str) -> Option<String> {
    let (scheme, _) = url.trim().split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then(|| scheme.to_ascii_lowercase())
}

/// Whether the engine itself loads `url`: the web, local files and pages that carry their own
/// content. Any other scheme (`mailto:`, `tel:`, `ftp:`, an app's own) is for another app.
pub fn engine_loads(url: &str) -> bool {
    matches!(
        scheme(url).as_deref(),
        Some("http" | "https" | "file" | "data" | "blob" | "about" | "javascript")
    )
}

fn has_prefix(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| {
        text.get(..p.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(p))
    })
}

/// `data:` URL of an HTML page, percent-encoded (the view reads `data:`
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

    fn host(text: &str) -> Typed {
        classify(text, DEFAULT_SEARCH).unwrap()
    }

    fn search(query: &str) -> Typed {
        Typed::Search(format!("https://duckduckgo.com/?q={query}"))
    }

    #[test]
    fn bare_hosts_become_https_urls_with_an_http_twin() {
        assert_eq!(normalize("example.com").unwrap(), "https://example.com/");
        assert_eq!(
            host("  Example.com/a b "),
            Typed::Host {
                host: "example.com".into(),
                https: "https://Example.com/a b".into(),
                http: "http://Example.com/a b".into(),
            }
        );
        assert_eq!(
            normalize("localhost:8080").unwrap(),
            "https://localhost:8080/"
        );
        assert_eq!(
            normalize("10.0.2.2:8080?x").unwrap(),
            "https://10.0.2.2:8080?x"
        );
        assert_eq!(normalize("[::1]:8080").unwrap(), "https://[::1]:8080/");
        assert_eq!(normalize("[fe80::1]/x").unwrap(), "https://[fe80::1]/x");
        assert_eq!(normalize("intranet/wiki").unwrap(), "https://intranet/wiki");
        assert_eq!(
            normalize("example.com/go?to=https://x.org").unwrap(),
            "https://example.com/go?to=https://x.org"
        );
        assert_eq!(normalize("localhost").unwrap(), "https://localhost/");
        assert_eq!(
            normalize("xn--nxasmq6b.example").unwrap(),
            "https://xn--nxasmq6b.example/"
        );
    }

    #[test]
    fn anything_else_is_a_search() {
        assert_eq!(host("rust lifetimes"), search("rust%20lifetimes"));
        assert_eq!(host("weather"), search("weather"));
        assert_eq!(
            host("what is 1.5 + 2?"),
            search("what%20is%201.5%20%2B%202%3F")
        );
        // A dot in a sentence, or a number with a dot, is not a host.
        assert_eq!(host("version 2.0"), search("version%202.0"));
        assert_eq!(host("3.14"), search("3.14"));
        assert_eq!(host("e.g."), search("e.g."));
        assert_eq!(host("user@example.com"), search("user%40example.com"));
        assert_eq!(
            host("example.com is down"),
            search("example.com%20is%20down")
        );
        assert_eq!(host("note:5"), search("note%3A5"));
        assert_eq!(host("host:99999"), search("host%3A99999"));
        assert_eq!(host("1.2.3.256"), search("1.2.3.256"));
        assert_eq!(host("weather?"), search("weather%3F"));
    }

    #[test]
    fn a_leading_question_mark_always_searches() {
        assert_eq!(host("?example.com"), search("example.com"));
        assert_eq!(host("  ? a&b=c "), search("a%26b%3Dc"));
        assert_eq!(classify("?", DEFAULT_SEARCH), None);
        assert_eq!(classify("?   ", DEFAULT_SEARCH), None);
    }

    #[test]
    fn a_bad_template_falls_back_to_the_default_and_a_good_one_is_used() {
        let own = "https://search.example/find?q=%s&lang=en";
        assert_eq!(
            classify("two words", own).unwrap().url(),
            "https://search.example/find?q=two%20words&lang=en"
        );
        for bad in [
            "ftp://x/%s",
            "https://x/",
            "javascript:%s",
            "https://x/ %s",
            "%s",
        ] {
            assert!(!valid_template(bad), "{bad}");
            assert_eq!(
                classify("a b", bad).unwrap().url(),
                "https://duckduckgo.com/?q=a%20b"
            );
        }
        assert!(valid_template(DEFAULT_SEARCH));
        assert!(!valid_template(&format!("https://x/{}%s", "a".repeat(600))));
    }

    #[test]
    fn a_query_cannot_break_out_of_the_template() {
        let url = classify("a&b#c\"d", "https://s.example/?q=%s").unwrap();
        assert_eq!(url.url(), "https://s.example/?q=a%26b%23c%22d");
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
    fn schemes_are_read_in_lower_case_or_not_at_all() {
        assert_eq!(scheme("MAILTO:a@b").as_deref(), Some("mailto"));
        assert_eq!(scheme(" https://x/").as_deref(), Some("https"));
        assert_eq!(
            scheme("x-lazyweb:clear-history").as_deref(),
            Some("x-lazyweb")
        );
        assert_eq!(
            scheme("example.com/a:b"),
            None,
            "a colon after a slash is a path"
        );
        assert_eq!(scheme("1http://x"), None);
        assert_eq!(scheme("/tmp/a:b"), None);
        assert_eq!(scheme(":x"), None);
        assert_eq!(scheme(""), None);
    }

    #[test]
    fn the_engine_loads_the_web_and_files_and_everything_else_goes_to_another_app() {
        for url in [
            "http://a/",
            "HTTPS://a/",
            "file:///tmp/x.html",
            "data:text/html,x",
            "about:blank",
            "blob:http://a/1",
        ] {
            assert!(engine_loads(url), "{url}");
        }
        for url in [
            "mailto:a@b",
            "tel:123",
            "ftp://h/f",
            "x-lazyweb:clear-history",
            "irc://h/c",
            "example.com",
        ] {
            assert!(!engine_loads(url), "{url}");
        }
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
