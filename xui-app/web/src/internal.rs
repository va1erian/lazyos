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
    pub fn name_for(&self, url: &str) -> Option<&'static str> {
        self.urls
            .iter()
            .find(|(_, built)| built.as_str() == url)
            .map(|(name, _)| *name)
    }

    /// `url` as the address bar shows it.
    pub fn shown<'a>(&self, url: &'a str) -> &'a str {
        self.name_for(url).unwrap_or(url)
    }
}
