//! Address bar suggestions from the browsing history: which visited pages match what is being
//! typed, best first. Pure and host-tested; nothing here touches the network or a window (the
//! dropdown's state is [`dropdown`], its widget [`widget`]).
//!
//! A page matches when every word typed is the start of a word of its title, of a label of its host
//! (`gist.github.com` for `github`), or of its address without the scheme and `www.`
//! (`example.com/doc` for `example.com/d`). Matches rank by visits times a recency weight
//! ([`recency`]), doubled when the typed text opens the address itself: a person who types `ex`
//! more often means `example.com` than a page whose title says "ex".

pub mod dropdown;
pub mod widget;

use std::collections::HashMap;

use crate::visits::Visit;

/// The most suggestions shown (the search row comes after them).
pub const MAX_ROWS: usize = 8;
/// The most words of the query that count; the rest are ignored.
const MAX_WORDS: usize = 6;
/// The longest query ranked (a pasted address of any length is cut, not scanned for).
const MAX_QUERY: usize = 256;

const DAY: u64 = 86_400;

/// One visited page, once, with how often and when it was visited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub url: String,
    pub title: String,
    pub visits: u32,
    /// Seconds since the Unix epoch of the newest visit.
    pub last: u64,
    /// The address without scheme and `www.`, lower case: what typed text is matched against.
    key: String,
    /// Where the host ends in `key`.
    host_len: usize,
    title_lower: String,
}

/// The pages of a history, ready to be ranked.
#[derive(Clone, Debug, Default)]
pub struct Index {
    items: Vec<Item>,
}

/// A suggestion: a page of the index.
pub type Match<'a> = &'a Item;

impl Item {
    fn new(url: &str, title: &str, visits: u32, last: u64) -> Item {
        let key = key_of(url);
        let host_len = key.find(['/', '?', '#']).unwrap_or(key.len());
        Item {
            url: url.to_owned(),
            title: title.to_owned(),
            visits,
            last,
            key,
            host_len,
            title_lower: title.to_lowercase(),
        }
    }

    /// The address of `url` as the dropdown shows it: no scheme, no `www.`, no trailing `/`.
    pub fn short_of(url: &str) -> String {
        let key = key_of(url);
        key.strip_suffix('/').map(str::to_owned).unwrap_or(key)
    }

    /// The host (and port) of the page, lower case.
    pub fn host(&self) -> &str {
        &self.key[..self.host_len]
    }

    /// Whether the typed `word` (lower case) starts something this page is known by.
    fn matches_word(&self, word: &str) -> bool {
        self.key.starts_with(word)
            || self.host().split('.').any(|label| label.starts_with(word))
            || self
                .title_lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|title_word| title_word.starts_with(word))
    }
}

/// `url` lower case without its scheme and `www.`: what typed text is matched against.
fn key_of(url: &str) -> String {
    let lower = url.to_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .or_else(|| lower.strip_prefix("file://"))
        .unwrap_or(&lower);
    rest.strip_prefix("www.").unwrap_or(rest).to_owned()
}

/// How much a visit `age` seconds ago counts, in percent: a page seen today outweighs three seen
/// last month.
pub fn recency(age: u64) -> u64 {
    match age {
        a if a < DAY => 100,
        a if a < 7 * DAY => 70,
        a if a < 30 * DAY => 40,
        a if a < 90 * DAY => 20,
        _ => 10,
    }
}

impl Index {
    /// The pages of `visits`, each once with its number of visits, its newest time and its newest
    /// non-empty title.
    pub fn from_visits(visits: &[Visit]) -> Index {
        let mut slot: HashMap<&str, usize> = HashMap::new();
        let mut items: Vec<(&str, String, u32, u64)> = Vec::new();
        for visit in visits {
            match slot.get(visit.url.as_str()) {
                Some(&at) => {
                    let item = &mut items[at];
                    item.2 = item.2.saturating_add(1);
                    if visit.time >= item.3 {
                        item.3 = visit.time;
                    }
                    if !visit.title.is_empty() {
                        item.1.clone_from(&visit.title);
                    }
                }
                None => {
                    slot.insert(visit.url.as_str(), items.len());
                    items.push((visit.url.as_str(), visit.title.clone(), 1, visit.time));
                }
            }
        }
        Index {
            items: items
                .into_iter()
                .map(|(url, title, count, last)| Item::new(url, &title, count, last))
                .collect(),
        }
    }

    /// An index of `items` given directly (the bench's).
    pub fn from_items(items: impl IntoIterator<Item = (String, String, u32, u64)>) -> Index {
        Index {
            items: items
                .into_iter()
                .map(|(url, title, visits, last)| Item::new(&url, &title, visits, last))
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The best `limit` pages for `query` at time `now`, best first. Nothing for a query of no words.
    pub fn rank(&self, query: &str, now: u64, limit: usize) -> Vec<Match<'_>> {
        let query: String = query.chars().take(MAX_QUERY).collect();
        let query = query.to_lowercase();
        // `www.` is not part of the key (it is in the way of typing a host), so it is not part of a word.
        let words: Vec<&str> = query
            .split_whitespace()
            .take(MAX_WORDS)
            .map(|w| {
                w.strip_prefix("www.")
                    .filter(|r| !r.is_empty())
                    .unwrap_or(w)
            })
            .collect();
        if words.is_empty() || limit == 0 {
            return Vec::new();
        }
        let mut scored: Vec<(u64, usize)> = Vec::new();
        for (at, item) in self.items.iter().enumerate() {
            if !words.iter().all(|word| item.matches_word(word)) {
                continue;
            }
            let opens_address = item.key.starts_with(words[0]);
            let weight = recency(now.saturating_sub(item.last)) * if opens_address { 2 } else { 1 };
            scored.push((u64::from(item.visits) * weight, at));
        }
        // Best first; among equals the newer visit, then the earlier entry, so the order is stable.
        let better = |a: &(u64, usize), b: &(u64, usize)| {
            b.0.cmp(&a.0)
                .then_with(|| self.items[b.1].last.cmp(&self.items[a.1].last))
                .then_with(|| a.1.cmp(&b.1))
        };
        if scored.len() > limit {
            scored.select_nth_unstable_by(limit, better);
            scored.truncate(limit);
        }
        scored.sort_unstable_by(better);
        scored.into_iter().map(|(_, at)| &self.items[at]).collect()
    }

    /// Forgets the page `url`.
    pub fn remove(&mut self, url: &str) {
        self.items.retain(|item| item.url != url);
    }
}

/// The address field's text with the rest of the best match's host added, when what was typed is one
/// word that opens that host (`exa` and `example.com` give `example.com`); `None` when there is
/// nothing to add. The typed letters keep their case.
pub fn completion(query: &str, best: &Item) -> Option<String> {
    let typed = query.trim_start();
    if typed.is_empty() || typed.contains(char::is_whitespace) || typed.contains(['/', ':']) {
        return None;
    }
    let lower = typed.to_lowercase();
    let host = best.host();
    // The prefix must be in the host and leave something to add; byte offsets agree because the
    // match is on the lowercased text.
    let rest = host
        .strip_prefix(lower.as_str())
        .filter(|r| !r.is_empty())?;
    Some(format!("{typed}{rest}"))
}

#[cfg(test)]
mod tests;
