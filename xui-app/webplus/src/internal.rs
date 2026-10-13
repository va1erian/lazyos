//! The browser's own pages by name: the start page and the `about:` pages of [`crate::pages`],
//! each shown as a `data:` URL that is built when it is opened. The address bar, the markers and
//! the history use the name; only the engine sees the `data:` URL (WebKit's C API has no custom
//! scheme handler, and a `data:` URL is also what Back and Forward can reload).
//!
//! The registry is shared with the navigation policy, which runs on the engine's thread and asks
//! whether the page on screen is one of these ([`Internal::name_for`]): a web page cannot pass
//! for one, because only the exact URL built here counts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::address::{self, START};
use crate::pages::{ABOUT, DOWNLOADS, HISTORY};

/// The built-in start page.
const START_HTML: &str = include_str!("start.html");

/// The pages the browser builds itself.
pub const NAMES: [&str; 4] = [START, HISTORY, DOWNLOADS, ABOUT];

/// The `data:` URL last built for each page.
#[derive(Clone, Default)]
pub struct Internal {
    urls: Arc<Mutex<HashMap<&'static str, String>>>,
}

impl Internal {
    fn urls(&self) -> MutexGuard<'_, HashMap<&'static str, String>> {
        self.urls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The page `name` (one of [`NAMES`], any case), if it is one.
    pub fn name_of(text: &str) -> Option<&'static str> {
        NAMES
            .iter()
            .copied()
            .find(|name| name.eq_ignore_ascii_case(text.trim()))
    }

    /// Builds page `name` from `html` and returns its URL.
    pub fn build(&self, name: &'static str, html: &str) -> String {
        let url = address::html_data_url(html);
        self.urls().insert(name, url.clone());
        url
    }

    /// The start page's URL (built once: it never changes).
    pub fn start(&self) -> String {
        let built = self.urls().get(START).cloned();
        built.unwrap_or_else(|| self.build(START, START_HTML))
    }

    /// The page's name for a URL the engine reported, if it is one of ours.
    /// The engine may normalize the URL (it can escape `<`, `>`, `"` and spaces that
    /// ours leaves bare), so the two are compared once fully unescaped.
    pub fn name_for(&self, url: &str) -> Option<&'static str> {
        if !url.starts_with("data:") {
            return None;
        }
        let reported = unescape(url);
        self.urls()
            .iter()
            .find(|(_, built)| unescape(built) == reported)
            .map(|(name, _)| *name)
    }

    /// The URL to open for `shown` (a page's name, or any URL).
    pub fn url_of(&self, shown: &str) -> String {
        Internal::name_of(shown)
            .and_then(|name| self.urls().get(name).cloned())
            .unwrap_or_else(|| shown.to_string())
    }

    /// `url` as the address bar shows it.
    pub fn shown<'a>(&self, url: &'a str) -> &'a str {
        self.name_for(url).unwrap_or(url)
    }
}

/// `url` with every `%XX` escape decoded.
fn unescape(url: &str) -> Vec<u8> {
    let bytes = url.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_found_by_its_normalized_url() {
        let internal = Internal::default();
        let url = internal.build(HISTORY, "<p class=\"a\">two words</p>");
        let normalized = url
            .replace('<', "%3C")
            .replace('>', "%3E")
            .replace('"', "%22")
            .replace(' ', "%20");
        assert_eq!(internal.name_for(&url), Some(HISTORY));
        assert_eq!(internal.name_for(&normalized), Some(HISTORY));
        assert_eq!(internal.shown(&normalized), HISTORY);
        assert_eq!(internal.name_for("https://example.com/"), None);
        assert_eq!(internal.url_of(HISTORY), url);
        assert_eq!(
            internal.url_of("https://example.com/"),
            "https://example.com/"
        );
    }

    #[test]
    fn a_page_that_only_looks_like_ours_is_not_ours() {
        let internal = Internal::default();
        internal.build(HISTORY, "<p>history</p>");
        // The same shape, one character different: a web page's own data: URL.
        let forged = address::html_data_url("<p>history</p><a href=\"x-lazyweb:clear-history\">");
        assert_eq!(internal.name_for(&forged), None);
        // An http page is never one, whatever it contains.
        assert_eq!(internal.name_for("http://evil/data:text/html,x"), None);
    }

    #[test]
    fn the_registry_is_shared_with_the_clones_the_policy_holds() {
        let internal = Internal::default();
        let policy = internal.clone();
        let url = internal.start();
        assert_eq!(policy.name_for(&url), Some(START));
        let history = internal.build(HISTORY, "<p>h</p>");
        assert_eq!(policy.name_for(&history), Some(HISTORY));
        // Rebuilding a page replaces its URL: the old one is no longer ours.
        let newer = internal.build(HISTORY, "<p>h2</p>");
        assert_eq!(policy.name_for(&newer), Some(HISTORY));
        assert_eq!(policy.name_for(&history), None);
    }

    #[test]
    fn names_are_found_in_any_case() {
        assert_eq!(Internal::name_of(" ABOUT:History "), Some(HISTORY));
        assert_eq!(Internal::name_of("about:lazyweb"), Some(ABOUT));
        assert_eq!(Internal::name_of("about:blank"), None);
    }

    #[test]
    fn unescape_leaves_broken_escapes() {
        assert_eq!(unescape("a%3Cb%2"), b"a<b%2");
        assert_eq!(unescape("%zz%41"), b"%zzA");
    }
}
