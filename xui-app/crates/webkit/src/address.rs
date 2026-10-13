//! What the address field turns typed text into: a URL with a scheme is taken
//! as is, an absolute path is a `file:` URL, anything else is a host reached
//! over plain HTTP (`example.com` opens `http://example.com/`).

/// Schemes taken as typed, without `//`.
const OPAQUE_SCHEMES: &[&str] = &["about:", "data:", "javascript:", "mailto:"];

/// The URL to open for the address field's `text`, or `None` when there is
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
    let slash = if text.contains(['/', '?', '#']) {
        ""
    } else {
        "/"
    };
    Some(format!("http://{text}{slash}"))
}

fn has_prefix(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| {
        text.get(..p.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(p))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_hosts_become_http_urls() {
        assert_eq!(normalize("example.com").unwrap(), "http://example.com/");
        assert_eq!(
            normalize("  example.com/a ").unwrap(),
            "http://example.com/a"
        );
        assert_eq!(
            normalize("localhost:8080").unwrap(),
            "http://localhost:8080/"
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
    fn nothing_to_open() {
        assert_eq!(normalize("   "), None);
        assert_eq!(normalize("a\u{7}b"), None);
    }
}
