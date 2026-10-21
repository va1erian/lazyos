//! What a click on a link in a document means. A light view loads nothing by itself: it reports the
//! link as written (a relative one stays relative) and the app decides.
//!
//! * `#section` stays on the page: a light view cannot scroll to it, so nothing happens;
//! * a link to another Markdown file opens it here, resolved against the folder of the document on
//!   show (the documentation root when the welcome page is on show);
//! * any other relative or absolute path is a file for `mimed` to open with its registered app;
//! * `http:`, `https:` and `mailto:` go to the app registered for the scheme (the browser, Mail);
//! * every other scheme (`javascript:`, `data:`, `file:`, ...) is refused.

use std::path::{Component, Path, PathBuf};

/// What to do with a clicked link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// A link within the page: nothing to do.
    Anchor,
    /// Another Markdown document: show it in this window.
    Document(PathBuf),
    /// Another kind of file: hand it to `mimed`.
    File(PathBuf),
    /// A web or mail address: hand it to the app registered for its scheme.
    Url(String),
    /// Nothing the viewer will follow (an empty link, a scheme it does not hand on).
    Refused,
}

/// Longest link the viewer looks at; a longer one is refused (a document can write any length).
pub const MAX_LINK: usize = 2048;

/// Classifies `href` as the page told it, for the document at `current` (or the welcome page).
pub fn classify(href: &str, current: Option<&Path>) -> Link {
    let href = href.trim();
    if href.is_empty() || href.len() > MAX_LINK || href.chars().any(char::is_control) {
        return Link::Refused;
    }
    if href.starts_with('#') {
        return Link::Anchor;
    }
    if let Some(scheme) = scheme_of(href) {
        return match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" | "mailto" => Link::Url(href.to_owned()),
            _ => Link::Refused,
        };
    }
    let path = href.split(['#', '?']).next().unwrap_or("");
    let Some(path) = percent_decode(path) else {
        return Link::Refused;
    };
    if path.is_empty() {
        return Link::Anchor;
    }
    let base = current
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new(fhs::docs::DOCS_ROOT));
    let target = normalize(&base.join(&path));
    if is_markdown(&target) {
        Link::Document(target)
    } else {
        Link::File(target)
    }
}

/// The scheme of `href` (`https` of `https://x`), or `None` for a path: letters, digits, `+`, `-`
/// and `.` after a letter, then `:` before any `/`, `?` or `#`.
fn scheme_of(href: &str) -> Option<&str> {
    let end = href.find([':', '/', '?', '#'])?;
    let (scheme, rest) = href.split_at(end);
    let valid = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    (rest.starts_with(':') && valid).then_some(scheme)
}

/// `%20` and the like decoded; `None` when the bytes are not UTF-8 or an escape is cut short.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = bytes.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `path` with `.` and `..` folded away (no file system involved; `..` at the root stays at the root).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                if out.parent().is_some() {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: &str = "/docs/os/guide/intro.md";

    fn classify_in(href: &str) -> Link {
        classify(href, Some(Path::new(CURRENT)))
    }

    #[test]
    fn an_anchor_stays_on_the_page() {
        assert_eq!(classify_in("#section"), Link::Anchor);
        assert_eq!(classify_in("intro.md#section"), Link::Document("/docs/os/guide/intro.md".into()));
        assert_eq!(classify_in("?x=1"), Link::Anchor);
    }

    #[test]
    fn a_relative_markdown_link_opens_beside_the_current_document() {
        assert_eq!(classify_in("other.md"), Link::Document("/docs/os/guide/other.md".into()));
        assert_eq!(classify_in("./sub/a.MD"), Link::Document("/docs/os/guide/sub/a.MD".into()));
        assert_eq!(classify_in("../index.markdown"), Link::Document("/docs/os/index.markdown".into()));
        assert_eq!(classify_in("a%20b.md"), Link::Document("/docs/os/guide/a b.md".into()));
    }

    #[test]
    fn dot_dot_cannot_leave_the_root() {
        assert_eq!(classify_in("../../../../../x.md"), Link::Document("/x.md".into()));
    }

    #[test]
    fn an_absolute_path_is_taken_as_written() {
        assert_eq!(classify_in("/docs/apps/a.md"), Link::Document("/docs/apps/a.md".into()));
        assert_eq!(classify_in("/system/share/samples/x.pdf"), Link::File("/system/share/samples/x.pdf".into()));
    }

    #[test]
    fn the_welcome_page_resolves_against_the_documentation_root() {
        let Link::Document(path) = classify("os/readme.md", None) else {
            panic!("a document");
        };
        assert_eq!(path, Path::new(fhs::docs::DOCS_ROOT).join("os/readme.md"));
    }

    #[test]
    fn other_files_go_to_mimed() {
        assert_eq!(classify_in("diagram.png"), Link::File("/docs/os/guide/diagram.png".into()));
    }

    #[test]
    fn web_and_mail_addresses_are_handed_on_and_other_schemes_refused() {
        assert_eq!(classify_in("https://example.com/a?b#c"), Link::Url("https://example.com/a?b#c".into()));
        assert_eq!(classify_in("HTTP://x"), Link::Url("HTTP://x".into()));
        assert_eq!(classify_in("mailto:a@b.c"), Link::Url("mailto:a@b.c".into()));
        for refused in ["javascript:alert(1)", "data:text/html,x", "file:///etc/passwd", "x-lazyweb:about"] {
            assert_eq!(classify_in(refused), Link::Refused, "{refused}");
        }
    }

    #[test]
    fn nonsense_is_refused() {
        assert_eq!(classify_in(""), Link::Refused);
        assert_eq!(classify_in("   "), Link::Refused);
        assert_eq!(classify_in("a\nb.md"), Link::Refused);
        assert_eq!(classify_in("%zz.md"), Link::Refused);
        assert_eq!(classify_in("%ff.md"), Link::Refused, "not UTF-8");
        assert_eq!(classify_in(&"a".repeat(MAX_LINK + 1)), Link::Refused);
    }

    #[test]
    fn a_colon_after_a_slash_is_not_a_scheme() {
        assert_eq!(classify_in("a/b:c.md"), Link::Document("/docs/os/guide/a/b:c.md".into()));
    }
}
