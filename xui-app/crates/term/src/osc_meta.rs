//! The OSC commands about the window and its text rather than colours:
//! titles (0, 2), the working directory (7) and hyperlinks (8). Everything
//! here comes from a program's output, so every piece is bounded and
//! stripped of control characters before it is kept.

use crate::state::State;

/// The longest title kept, in characters.
pub const MAX_TITLE: usize = 256;
/// The longest working-directory path kept, in bytes.
pub const MAX_CWD: usize = 2048;
/// The longest host kept.
const MAX_HOST: usize = 255;

/// The working directory an OSC 7 gave: `file://<host>/<path>`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cwd {
    pub host: String,
    pub path: String,
}

/// The text of an OSC parameter, `None` when it is not UTF-8.
fn utf8(param: &[u8]) -> Option<&str> {
    std::str::from_utf8(param).ok()
}

/// A title as the Terminal shows it: printable characters only, at most
/// [`MAX_TITLE`] of them. Empty after cleaning means no title.
pub fn clean_title(text: &str) -> Option<String> {
    let title: String = text.chars().filter(|c| !c.is_control()).take(MAX_TITLE).collect();
    let title = title.trim().to_string();
    (!title.is_empty()).then_some(title)
}

fn hex(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).map(|d| d as u8)
}

/// Decode `%XX` escapes; `None` when one is malformed or the result is not
/// UTF-8 or holds a control character.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let (hi, lo) = (hex(*bytes.get(at + 1)?)?, hex(*bytes.get(at + 2)?)?);
            out.push(hi * 16 + lo);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    let decoded = String::from_utf8(out).ok()?;
    (!decoded.chars().any(|c| c.is_control())).then_some(decoded)
}

/// Parse an OSC 7 URL.
pub fn parse_cwd(url: &str) -> Option<Cwd> {
    let rest = url.strip_prefix("file://")?;
    let (host, path) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        // `file://~` (the shell's tilde) has no path of its own.
        None => (rest, ""),
    };
    if host.len() > MAX_HOST || host.chars().any(|c| c.is_control()) || path.len() > 3 * MAX_CWD {
        return None;
    }
    let path = percent_decode(path)?;
    (path.len() <= MAX_CWD).then(|| Cwd { host: host.to_string(), path })
}

impl State {
    /// OSC 0 and 2: the window title. An empty one clears it.
    pub(crate) fn set_title(&mut self, params: &[&[u8]]) {
        // The text may itself hold `;`, which the parser split on.
        let joined: Vec<&str> = params[1..].iter().filter_map(|p| utf8(p)).collect();
        let title = clean_title(&joined.join(";"));
        if self.title != title {
            self.title = title;
            self.title_generation += 1;
        }
    }

    /// OSC 7: the shell's working directory.
    pub(crate) fn set_cwd(&mut self, params: &[&[u8]]) {
        let joined: Vec<&str> = params[1..].iter().filter_map(|p| utf8(p)).collect();
        if let Some(cwd) = parse_cwd(&joined.join(";")) {
            self.cwd = Some(cwd);
        }
    }

    /// OSC 8: `8 ; params ; URI` starts a link, an empty URI ends it. The URI
    /// may hold `;`; the only parameter used is `id=`.
    pub(crate) fn set_link(&mut self, params: &[&[u8]]) {
        let [_, options, uri @ ..] = params else {
            return;
        };
        let uri: Vec<&str> = uri.iter().filter_map(|p| utf8(p)).collect();
        let uri = uri.join(";");
        if uri.is_empty() {
            self.attr.link = 0;
            return;
        }
        let id = utf8(options)
            .unwrap_or("")
            .split(':')
            .find_map(|option| option.strip_prefix("id="))
            .unwrap_or("");
        self.attr.link = self.links.intern(id, &uri);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_cleaned_and_bounded() {
        assert_eq!(clean_title("hello"), Some("hello".to_string()));
        assert_eq!(clean_title("  spaced  "), Some("spaced".to_string()));
        assert_eq!(clean_title("a\x1b[31mb\x07c\nd"), Some("a[31mbcd".to_string()), "controls dropped");
        assert_eq!(clean_title(""), None);
        assert_eq!(clean_title("\x01\x02"), None);
        let long = "x".repeat(10_000);
        assert_eq!(clean_title(&long).unwrap().chars().count(), MAX_TITLE);
        let wide = "\u{4e2d}".repeat(1000);
        assert_eq!(clean_title(&wide).unwrap().chars().count(), MAX_TITLE);
    }

    #[test]
    fn working_directories() {
        let ok = |url: &str| parse_cwd(url).map(|c| (c.host, c.path));
        assert_eq!(ok("file:///home/user"), Some((String::new(), "/home/user".to_string())));
        assert_eq!(ok("file://lazyos/tmp/a%20b"), Some(("lazyos".to_string(), "/tmp/a b".to_string())));
        assert_eq!(ok("file://~/src"), Some(("~".to_string(), "/src".to_string())));
        assert_eq!(ok("file://~"), Some(("~".to_string(), String::new())));
        assert_eq!(ok("file:///caf%C3%A9"), Some((String::new(), "/caf\u{e9}".to_string())));
        for bad in [
            "http://x/y",
            "file:/x",
            "",
            "file:///a%zz",
            "file:///a%",
            "file:///a%0Ab",
            "file:///a%00b",
            "file:///a%FF",
            "file:///a\nb",
        ] {
            assert_eq!(ok(bad), None, "{bad:?}");
        }
        let huge = format!("file:///{}", "a".repeat(10_000));
        assert_eq!(ok(&huge), None);
        let long_host = format!("file://{}/x", "h".repeat(300));
        assert_eq!(ok(&long_host), None);
    }
}
