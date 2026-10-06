//! LazyWeb's own pages by name: the start page and the `about:` pages of
//! [`lazyweb::pages`], each shown as a `data:` URL that is built when it is
//! opened. The address bar, the Back/Forward list and the markers use the
//! name; only the view sees the `data:` URL.

use std::collections::HashMap;

use lazyweb::address::{self, START};
use lazyweb::pages::{ABOUT, DOWNLOADS, HISTORY};

/// The built-in start page.
const START_HTML: &str = include_str!("start.html");

/// The pages LazyWeb builds itself.
pub const NAMES: [&str; 4] = [START, HISTORY, DOWNLOADS, ABOUT];

/// The `data:` URL last built for each page.
#[derive(Default)]
pub struct Internal {
    urls: HashMap<&'static str, String>,
}

impl Internal {
    /// The page `name` (one of [`NAMES`], any case), if it is one.
    pub fn name_of(text: &str) -> Option<&'static str> {
        NAMES
            .iter()
            .copied()
            .find(|name| name.eq_ignore_ascii_case(text.trim()))
    }

    /// Builds page `name` from `html` and returns its URL.
    pub fn build(&mut self, name: &'static str, html: &str) -> String {
        let url = address::html_data_url(html);
        self.urls.insert(name, url.clone());
        url
    }

    /// The start page's URL (built once: it never changes).
    pub fn start(&mut self) -> String {
        match self.urls.get(START) {
            Some(url) => url.clone(),
            None => self.build(START, START_HTML),
        }
    }

    /// The page's name for a URL the view reported, if it is one of ours.
    /// NetSurf normalizes the URL (it escapes `<`, `>`, `"` and spaces that
    /// ours leaves bare), so the two are compared once fully unescaped.
    pub fn name_for(&self, url: &str) -> Option<&'static str> {
        if !url.starts_with("data:") {
            return None;
        }
        let reported = unescape(url);
        self.urls
            .iter()
            .find(|(_, built)| unescape(built) == reported)
            .map(|(name, _)| *name)
    }

    /// The URL to open for `shown` (a page's name, or any URL).
    pub fn url_of(&self, shown: &str) -> String {
        Internal::name_of(shown)
            .and_then(|name| self.urls.get(name))
            .cloned()
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
        let mut internal = Internal::default();
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
    fn unescape_leaves_broken_escapes() {
        assert_eq!(unescape("a%3Cb%2"), b"a<b%2");
        assert_eq!(unescape("%zz%41"), b"%zzA");
    }
}
