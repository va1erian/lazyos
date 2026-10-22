//! Whose notification it is, and what a page's words become before they reach the shell.
//!
//! A page's origin is what its permission belongs to (`https://example.com`, with a port when it
//! is not the scheme's own); only `http:` and `https:` pages have one a person can allow. The toast
//! is titled with the host (never the page's own text), and the page's `title` and `body` are
//! cleaned and trimmed to the shell's limits (`idl/notify.midl`): nothing a page sends can make a
//! toast look like something else.

/// The shell's longest toast title, in characters.
pub const TITLE_MAX: usize = 64;
/// The shell's longest toast body, in characters.
pub const BODY_MAX: usize = 256;
/// The page's own title, before the body's share is taken: a long title leaves the body room.
const PAGE_TITLE_MAX: usize = 80;
/// The longest host name, as DNS has it.
const HOST_MAX: usize = 253;

/// A web origin: scheme, host and port, in the form WebKit writes it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    secure: bool,
    host: String,
    port: Option<u16>,
}

impl Origin {
    /// The origin of `url` when it is an `http:` or `https:` address with a host: user and
    /// password, path, query and fragment are not part of it. `None` for everything else
    /// (`data:`, `about:`, `file:`, `blob:` ...: pages that have no site to allow).
    pub fn from_url(url: &str) -> Option<Origin> {
        let url = url.trim();
        let (scheme, rest) = url.split_once("://")?;
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            _ => return None,
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host_port = authority.rsplit('@').next().unwrap_or(authority);
        let (host, port) = split_port(host_port)?;
        let host = host.to_ascii_lowercase();
        if !valid_host(&host) {
            return None;
        }
        // The scheme's own port is not written: `https://a.test:443` is `https://a.test`.
        let port = port.filter(|port| *port != if secure { 443 } else { 80 });
        Some(Origin { secure, host, port })
    }

    /// The origin as WebKit writes it and the permission file keeps it: `https://example.com` or
    /// `http://example.com:8080`.
    pub fn key(&self) -> String {
        match self.port {
            Some(port) => format!("{}://{}:{port}", self.scheme(), self.host),
            None => format!("{}://{}", self.scheme(), self.host),
        }
    }

    /// What the person is shown: the host, with the port when it is not the scheme's own.
    pub fn display(&self) -> String {
        match self.port {
            Some(port) => format!("{}:{port}", self.host),
            None => self.host.clone(),
        }
    }

    /// What the permission bar says: the host (as [`toast_title`] keeps it), with `http://` before
    /// it when the site is not secure, so `http://a.test` is not taken for `https://a.test`.
    pub fn bar_text(&self) -> String {
        let host = toast_title(self);
        if self.secure {
            host
        } else {
            format!("http://{host}")
        }
    }

    /// Whether `url` is a page of this origin.
    pub fn owns(&self, url: &str) -> bool {
        Origin::from_url(url).as_ref() == Some(self)
    }

    fn scheme(&self) -> &'static str {
        if self.secure {
            "https"
        } else {
            "http"
        }
    }
}

/// `host` and an optional port from `host[:port]` or `[v6][:port]`; `None` when the port is not a
/// number.
fn split_port(authority: &str) -> Option<(&str, Option<u16>)> {
    if authority.starts_with('[') {
        let (host, after) = authority.split_at(authority.find(']')? + 1);
        return match after {
            "" => Some((host, None)),
            _ => Some((host, Some(after.strip_prefix(':')?.parse().ok()?))),
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host, Some(port.parse().ok()?))),
        None => Some((authority, None)),
    }
}

/// A host that is a plain DNS name, an IPv4 address or a bracketed IPv6 address: no space, control
/// character or anything a person could not read as part of an address.
fn valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > HOST_MAX {
        return false;
    }
    if host.starts_with('[') {
        return host.ends_with(']')
            && host[1..host.len() - 1]
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
    }
    host.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
}

/// `text` with every control, bidirectional-override and zero-width character removed (a newline
/// or a tab is a space), runs of white space collapsed and the ends trimmed.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = true;
    for c in text.chars() {
        let c = if c.is_whitespace() || c.is_control() {
            ' '
        } else {
            c
        };
        if c == ' ' {
            if !space {
                out.push(' ');
            }
            space = true;
        } else if !invisible(c) {
            out.push(c);
            space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Format characters a page could use to reorder or hide text.
fn invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}')
}

/// `text` cut to `max` characters, ending in `...` when something was cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", kept.trim_end())
}

/// The toast's title: the origin's display name. A host too long for the title keeps its end (the
/// registrable part a person reads), never the start a spoof would fill.
pub fn toast_title(origin: &Origin) -> String {
    let shown = origin.display();
    let count = shown.chars().count();
    if count <= TITLE_MAX {
        return shown;
    }
    let tail: String = shown.chars().skip(count - (TITLE_MAX - 3)).collect();
    format!("...{tail}")
}

/// The toast's body: the page's `title` and `body` as two lines (a line is left out when the page
/// sent none), each cleaned, the whole within the shell's body limit.
pub fn toast_body(title: &str, body: &str) -> String {
    let title = cut(&clean(title), PAGE_TITLE_MAX);
    let body = clean(body);
    if body.is_empty() {
        return title;
    }
    if title.is_empty() {
        return cut(&body, BODY_MAX);
    }
    // One character for the line break.
    let room = BODY_MAX - title.chars().count() - 1;
    format!("{title}\n{}", cut(&body, room.max(4)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(url: &str) -> Origin {
        Origin::from_url(url).unwrap_or_else(|| panic!("{url} has an origin"))
    }

    #[test]
    fn an_origin_is_scheme_host_and_a_port_only_when_it_is_not_the_default() {
        let o = origin("https://user:pw@Host.Example:8443/path?q#f");
        assert_eq!(o.key(), "https://host.example:8443");
        assert_eq!(o.display(), "host.example:8443");
        assert_eq!(origin("https://a.test:443/").key(), "https://a.test");
        assert_eq!(origin("http://a.test:80").display(), "a.test");
        assert_eq!(origin("http://a.test:8080/").key(), "http://a.test:8080");
        assert_eq!(origin("http://[::1]:3000/x").key(), "http://[::1]:3000");
    }

    #[test]
    fn the_bar_names_an_insecure_site_by_its_scheme() {
        assert_eq!(origin("https://a.test:8443/").bar_text(), "a.test:8443");
        assert_eq!(origin("http://a.test/").bar_text(), "http://a.test");
    }

    #[test]
    fn only_web_pages_have_an_origin_a_person_can_allow() {
        for url in [
            "data:text/html,x",
            "about:blank",
            "about:lazyweb",
            "file:///etc/passwd",
            "blob:https://a.test/uuid",
            "javascript:alert(1)",
            "mailto:a@b.test",
            "x-lazyweb:clear-history",
            "ftp://a.test/",
            "https://",
            "https:///path",
            "https://a b.test/",
            "https://a.test:notaport/",
            "https://a.test:99999/",
            "https://[<script>]/",
            "",
        ] {
            assert_eq!(Origin::from_url(url), None, "{url}");
        }
    }

    #[test]
    fn a_site_owns_its_own_pages_only() {
        let o = origin("https://a.test");
        assert!(o.owns("https://a.test/deep/page?x=1"));
        assert!(!o.owns("http://a.test/"));
        assert!(!o.owns("https://a.test:8443/"));
        assert!(!o.owns("https://evil.test/https://a.test/"));
        assert!(!o.owns("https://a.test.evil.test/"));
        assert!(!o.owns("data:text/html,https://a.test/"));
    }

    #[test]
    fn a_page_cannot_smuggle_controls_or_reordering_into_its_words() {
        assert_eq!(clean("  hello \r\n\t world  "), "hello world");
        assert_eq!(clean("a\u{202E}b\u{200B}c\u{0007}d"), "abc d");
        assert_eq!(clean("\u{FEFF}\u{2066}x\u{2069}"), "x");
        assert_eq!(clean("\n\n"), "");
        assert_eq!(clean("caf\u{e9} \u{1f600}"), "caf\u{e9} \u{1f600}");
    }

    #[test]
    fn the_title_is_the_site_and_a_spoof_keeps_its_real_end() {
        assert_eq!(toast_title(&origin("https://example.com")), "example.com");
        let long = format!("https://accounts.paypal.com.{}.evil.test", "x".repeat(60));
        let title = toast_title(&origin(&long));
        assert_eq!(title.chars().count(), TITLE_MAX);
        assert!(
            title.starts_with("...") && title.ends_with(".evil.test"),
            "{title}"
        );
    }

    #[test]
    fn the_body_is_the_pages_title_and_text_as_two_lines_within_the_limit() {
        assert_eq!(toast_body("New mail", "From Ada"), "New mail\nFrom Ada");
        assert_eq!(toast_body("", "only body"), "only body");
        assert_eq!(toast_body("only title", "  "), "only title");
        assert_eq!(toast_body(" a \n b ", "c\r\nd"), "a b\nc d");
        let long = toast_body(&"t".repeat(300), &"b".repeat(300));
        assert!(long.chars().count() <= BODY_MAX, "{}", long.chars().count());
        let (title, body) = long.split_once('\n').unwrap();
        assert_eq!(title.chars().count(), PAGE_TITLE_MAX);
        assert!(title.ends_with("...") && body.ends_with("..."));
        assert!(!long.chars().any(|c| c.is_control() && c != '\n'));
    }
}
