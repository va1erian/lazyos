//! Moving between documents: where a link leads and the back/forward history.
//!
//! Pure and host-tested. The page view is given the document's own
//! `file://` URL as its base, so Blitz resolves every relative link
//! (`controls.md`, `../rhai/msg.md#errors`, `#heading`) to an absolute URL
//! before it reports the click; [`classify`] turns that URL back into a path.

use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use url::Url;

/// Most entries each of the back and forward lists keeps.
const HISTORY_LIMIT: usize = 100;

/// Where a clicked link leads.
#[derive(Debug, PartialEq, Eq)]
pub enum Link {
    /// A Markdown document (the path is the guest path, `/`-separated), at
    /// `fragment` when the link names a heading.
    Document {
        path: PathBuf,
        fragment: Option<String>,
    },
    /// A file that is not Markdown: Docs only renders Markdown.
    Other(String),
    /// Anything with another scheme (`https:`, `mailto:`): Docs has no
    /// network and does not launch other programs.
    External(String),
}

/// The `file://` URL a page is laid out against: relative links and `#`
/// fragments resolve against it. With no document (the welcome page) the
/// documentation root stands in, so its relative links work too.
pub fn base_url(path: Option<&Path>, fragment: Option<&str>) -> String {
    let mut url = Url::parse("file:///").expect("a constant URL parses");
    let path = path.map_or_else(|| format!("{}/", fhs::docs::OS_DOCS), |p| slashed(p));
    url.set_path(&path);
    url.set_fragment(fragment);
    url.into()
}

/// `path` with `/` separators, so a path built on a host reads like a guest's.
fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Sorts a URL Blitz reports for a click. `None` for one that is not a URL.
pub fn classify(href: &str) -> Option<Link> {
    let url = Url::parse(href).ok()?;
    if url.scheme() != "file" {
        return Some(Link::External(href.to_string()));
    }
    let path = percent_decode_str(url.path()).decode_utf8_lossy().into_owned();
    let markdown = Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"));
    if !markdown {
        return Some(Link::Other(path));
    }
    let fragment = url
        .fragment()
        .map(|f| percent_decode_str(f).decode_utf8_lossy().into_owned())
        .filter(|f| !f.is_empty());
    Some(Link::Document {
        path: PathBuf::from(path),
        fragment,
    })
}

/// A place in the documentation: a document (`None` is the welcome page).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub path: Option<PathBuf>,
}

/// The documents visited, for Back and Forward.
#[derive(Default)]
pub struct History {
    back: Vec<Place>,
    forward: Vec<Place>,
}

impl History {
    /// Records that the user left `from` for a new place: Forward is lost.
    pub fn visit(&mut self, from: Place) {
        if self.back.last() != Some(&from) {
            self.back.push(from);
            if self.back.len() > HISTORY_LIMIT {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    /// The place Back leads to from `current`, which Forward then returns to.
    pub fn back(&mut self, current: Place) -> Option<Place> {
        let target = self.back.pop()?;
        self.forward.push(current);
        Some(target)
    }

    /// The place Forward leads to from `current`.
    pub fn forward(&mut self, current: Place) -> Option<Place> {
        let target = self.forward.pop()?;
        self.back.push(current);
        Some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(path: &str) -> Place {
        Place {
            path: Some(PathBuf::from(path)),
        }
    }

    #[test]
    fn the_base_url_is_the_document_itself() {
        let url = base_url(Some(Path::new("/docs/os/lazyrad/index.md")), None);
        assert_eq!(url, "file:///docs/os/lazyrad/index.md");
        assert_eq!(base_url(None, None), "file:///docs/os/");
        assert_eq!(
            base_url(Some(Path::new("/docs/os/a b.md")), Some("top")),
            "file:///docs/os/a%20b.md#top"
        );
    }

    #[test]
    fn a_relative_link_resolves_against_the_base_and_classifies_as_a_document() {
        let base = Url::parse(&base_url(Some(Path::new("/docs/os/lazyrad/index.md")), None)).unwrap();
        let target = base.join("../rhai/msg.md#errors").unwrap();
        assert_eq!(
            classify(target.as_str()),
            Some(Link::Document {
                path: PathBuf::from("/docs/os/rhai/msg.md"),
                fragment: Some("errors".into()),
            })
        );
        let same = base.join("#forms").unwrap();
        assert_eq!(
            classify(same.as_str()),
            Some(Link::Document {
                path: PathBuf::from("/docs/os/lazyrad/index.md"),
                fragment: Some("forms".into()),
            })
        );
    }

    #[test]
    fn links_that_are_not_markdown_or_not_files_are_sorted_out() {
        assert_eq!(
            classify("file:///system/share/x.rhai"),
            Some(Link::Other("/system/share/x.rhai".into()))
        );
        assert_eq!(
            classify("https://example.com/a.md"),
            Some(Link::External("https://example.com/a.md".into()))
        );
        assert_eq!(classify("mailto:a@b.c"), Some(Link::External("mailto:a@b.c".into())));
        assert_eq!(classify("not a url"), None);
        assert_eq!(
            classify("file:///docs/os/a%20b.MD"),
            Some(Link::Document {
                path: PathBuf::from("/docs/os/a b.MD"),
                fragment: None
            })
        );
    }

    #[test]
    fn back_and_forward_walk_the_visited_documents() {
        let mut history = History::default();
        // welcome -> a -> b
        history.visit(Place { path: None });
        history.visit(doc("/a.md"));
        assert_eq!(history.back(doc("/b.md")), Some(doc("/a.md")));
        assert_eq!(history.back(doc("/a.md")), Some(Place { path: None }));
        assert_eq!(history.back(Place { path: None }), None);
        assert_eq!(history.forward(Place { path: None }), Some(doc("/a.md")));
        assert_eq!(history.forward(doc("/a.md")), Some(doc("/b.md")));
        assert_eq!(history.forward(doc("/b.md")), None);
    }

    #[test]
    fn a_new_visit_drops_forward_and_the_list_is_bounded() {
        let mut history = History::default();
        history.visit(doc("/a.md"));
        assert_eq!(history.back(doc("/b.md")), Some(doc("/a.md")));
        history.visit(doc("/a.md"));
        assert_eq!(history.forward(doc("/c.md")), None);
        for n in 0..HISTORY_LIMIT * 2 {
            history.visit(doc(&format!("/{n}.md")));
        }
        assert_eq!(history.back.len(), HISTORY_LIMIT);
    }
}
