//! Hyperlinks (OSC 8): each distinct (id, URI) pair is interned once and
//! cells carry its small index (`Attr::link`), so a page of linked text costs
//! two bytes a cell and one string. The table is bounded: a program cannot
//! grow it past [`MAX_LINKS`] entries or [`MAX_BYTES`] of text; a link that does
//! not fit is simply not made (the text prints unlinked).

use std::collections::HashMap;

/// The longest URI kept (a longer one is refused, not cut: a cut link goes
/// somewhere else).
pub const MAX_URI: usize = 2048;
/// The longest `id=` kept.
pub const MAX_ID: usize = 256;
/// The most distinct links one terminal remembers.
pub const MAX_LINKS: usize = 8192;
/// The most URI and id text, in bytes, all links together.
pub const MAX_BYTES: usize = 4 << 20;

#[derive(Default)]
pub struct Links {
    /// Entry `n` is link `n + 1`.
    entries: Vec<(String, String)>,
    index: HashMap<(String, String), u16>,
    bytes: usize,
}

/// Whether `text` is safe to store and later hand to the desktop: no control
/// characters, within `max` bytes.
fn clean(text: &str, max: usize) -> bool {
    text.len() <= max && !text.chars().any(|c| c.is_control())
}

impl Links {
    /// The index of the link (`id`, `uri`), made if new; `0` when it is not
    /// acceptable or the table is full.
    pub fn intern(&mut self, id: &str, uri: &str) -> u16 {
        if uri.is_empty() || !clean(uri, MAX_URI) || !clean(id, MAX_ID) {
            return 0;
        }
        let key = (id.to_string(), uri.to_string());
        if let Some(&found) = self.index.get(&key) {
            return found;
        }
        let cost = id.len() + uri.len();
        if self.entries.len() >= MAX_LINKS || self.bytes + cost > MAX_BYTES {
            return 0;
        }
        self.bytes += cost;
        self.entries.push(key.clone());
        let at = self.entries.len() as u16;
        self.index.insert(key, at);
        at
    }

    /// The URI of link `link`.
    pub fn uri(&self, link: u16) -> Option<&str> {
        let at = usize::from(link).checked_sub(1)?;
        self.entries.get(at).map(|(_, uri)| uri.as_str())
    }

    #[cfg(any(test, feature = "fuzz"))]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
