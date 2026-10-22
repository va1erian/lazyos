//! The browser's own pages that are too long to be an address webd takes (a `data:` URL over
//! [`URL_LIMIT`]: `about:history` with many visits) are given to webd as HTML (`LoadHtml`) at a short
//! base address of their own, and that address is told back to the window as the page's original
//! `data:` URL, so the window still knows its page (`Internal::name_for`) and its policy still
//! knows the page on screen is one of the browser's.
//!
//! The base carries a number chosen at start that a web page cannot guess, so no page can pass
//! for one of these by navigating to its base. A Back or Forward to such a page that WebKit has to
//! load again shows the base's own (empty) text: the window's Reload builds the page again.

use std::collections::hash_map::RandomState;
use std::collections::BTreeMap;
use std::hash::BuildHasher;

/// The longest `data:` page sent as an address (webd takes 8 KiB; the rest is headroom).
pub const URL_LIMIT: usize = 8000;
/// Aliases kept (the newest): a page older than that is no longer recognised as the browser's.
const KEPT: usize = 32;
/// What a page given as HTML starts with: only the browser's own pages are.
const HTML_PREFIX: &str = "data:text/html;charset=utf-8,";

#[derive(Debug)]
pub(crate) struct Aliases {
    /// Base address -> the page's original `data:` URL, with the order they were made.
    map: BTreeMap<String, (u64, String)>,
    next: u64,
    nonce: u64,
}

impl Default for Aliases {
    fn default() -> Self {
        Aliases {
            map: BTreeMap::new(),
            next: 1,
            nonce: RandomState::new().hash_one(std::time::SystemTime::now()),
        }
    }
}

impl Aliases {
    /// For a long `data:` page: its HTML and the base address to give it at.
    pub(crate) fn prepare(&mut self, url: &str) -> Option<(String, String)> {
        if url.len() <= URL_LIMIT {
            return None;
        }
        let encoded = url.strip_prefix(HTML_PREFIX)?;
        let html = String::from_utf8_lossy(&unescape(encoded)).into_owned();
        let n = self.next;
        self.next += 1;
        let base = format!("data:text/plain,lazyweb-page-{:016x}-{n}", self.nonce);
        self.map.insert(base.clone(), (n, url.to_owned()));
        if self.map.len() > KEPT {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, (made, _))| *made)
                .map(|(base, _)| base.clone());
            if let Some(oldest) = oldest {
                self.map.remove(&oldest);
            }
        }
        Some((html, base))
    }

    /// The address as the window knows it: a base address of ours is its page's `data:` URL.
    pub(crate) fn original(&self, url: &str) -> String {
        match self.map.get(url) {
            Some((_, original)) => original.clone(),
            None => url.to_owned(),
        }
    }
}

/// `text` with every `%XX` escape decoded.
fn unescape(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
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

    fn page(len: usize) -> String {
        format!("{HTML_PREFIX}%3Cp%3E{}%3C%2Fp%3E", "x".repeat(len))
    }

    #[test]
    fn a_short_page_stays_an_address() {
        let mut aliases = Aliases::default();
        assert_eq!(aliases.prepare(&page(10)), None);
        assert_eq!(aliases.prepare("https://example.com/"), None);
    }

    #[test]
    fn a_long_page_is_html_at_a_base_told_back_as_the_page() {
        let mut aliases = Aliases::default();
        let url = page(URL_LIMIT);
        let (html, base) = aliases.prepare(&url).unwrap();
        assert!(html.starts_with("<p>x") && html.ends_with("x</p>"));
        assert!(base.len() < 80 && base.starts_with("data:text/plain,lazyweb-page-"));
        assert_eq!(aliases.original(&base), url);
        assert_eq!(aliases.original("https://a.test/"), "https://a.test/");
        // A long `data:` URL that is not one of the browser's HTML pages is left alone.
        let other = format!("data:image/png;base64,{}", "A".repeat(URL_LIMIT));
        assert_eq!(aliases.prepare(&other), None);
    }

    #[test]
    fn only_the_newest_aliases_are_kept_and_bases_are_unique() {
        let mut aliases = Aliases::default();
        let mut bases = Vec::new();
        for _ in 0..(KEPT + 5) {
            bases.push(aliases.prepare(&page(URL_LIMIT)).unwrap().1);
        }
        let mut unique = bases.clone();
        unique.dedup();
        assert_eq!(unique.len(), bases.len());
        assert_eq!(aliases.original(&bases[0]), bases[0], "forgotten");
        assert_ne!(
            aliases.original(bases.last().unwrap()),
            *bases.last().unwrap()
        );
    }
}
