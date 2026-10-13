//! The filter lists the blocker knows, and turning a downloaded one into what the engine takes.
//!
//! A list is data fetched at runtime and never shipped (licences: `THIRD_PARTY.md`); its text is
//! converted by `lazyadblock` and the result is named by a hash of the source, so the engine's
//! store (which keeps compiled lists across runs) is looked up before anything is compiled.

use lazyadblock::{convert, Options, Stats};

/// A list the blocker can use.
#[derive(Debug, PartialEq, Eq)]
pub struct ListSpec {
    /// A short stable id: file names and stored settings use it.
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub licence: &'static str,
}

/// EasyList, the one list v1 ships a link to. More are a row here (and the licence note).
pub const EASYLIST: ListSpec = ListSpec {
    id: "easylist",
    name: "EasyList",
    url: "https://easylist.to/easylist/easylist.txt",
    licence: "GPL-3.0-or-later or CC BY-SA 3.0",
};

pub const LISTS: [ListSpec; 1] = [EASYLIST];

/// The list with this id.
pub fn spec(id: &str) -> Option<&'static ListSpec> {
    LISTS.iter().find(|l| l.id == id)
}

/// A name for the content of `text`: FNV-1a over its bytes, 16 hex digits. A new version of a
/// list is a new name, so a stored compiled list is never mistaken for it.
pub fn source_hash(text: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in text {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The id of part `n` of list `list` with source hash `hash`: what the engine's store knows it
/// by. Only `[a-z0-9-]`, so it is a safe file name.
pub fn part_id(list: &str, hash: &str, n: usize) -> String {
    format!("{list}-{hash}-{n}")
}

/// A converted list: its parts as `(id, json)` and what the conversion kept and dropped.
#[derive(Debug)]
pub struct Prepared {
    pub hash: String,
    pub parts: Vec<(String, String)>,
    pub stats: Stats,
}

impl Prepared {
    /// Rules across the parts.
    pub fn rules(&self) -> usize {
        self.stats.blocks + self.stats.exceptions + self.stats.important
    }

    /// Filters that did not become rules (element hiding included).
    pub fn dropped(&self) -> usize {
        self.stats.dropped_total()
    }
}

/// Converts the text of list `list` (as downloaded: bytes are read lossily, as a browser reads a
/// page). Cheap enough for a worker thread: EasyList is about 0.15 s in release.
pub fn prepare(list: &str, source: &[u8]) -> Prepared {
    let hash = source_hash(source);
    let text = String::from_utf8_lossy(source);
    let converted = convert(&text, &Options::default());
    let parts = converted
        .lists
        .into_iter()
        .enumerate()
        .map(|(n, part)| (part_id(list, &hash, n), part.json))
        .collect();
    Prepared {
        hash,
        parts,
        stats: converted.stats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_names_the_content() {
        assert_eq!(source_hash(b"").len(), 16);
        assert_eq!(source_hash(b"abc"), source_hash(b"abc"));
        assert_ne!(source_hash(b"abc"), source_hash(b"abd"));
        // FNV-1a 64 of "a".
        assert_eq!(source_hash(b"a"), "af63dc4c8601ec8c");
    }

    #[test]
    fn a_list_becomes_named_parts() {
        let text = b"[Adblock Plus 2.0]\n||ads.example^\n@@||ads.example/ok\nexample.com##.ad\n";
        let prepared = prepare("easylist", text);
        assert_eq!(prepared.parts.len(), 1);
        let (id, json) = &prepared.parts[0];
        assert_eq!(*id, format!("easylist-{}-0", source_hash(text)));
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
        assert!(json.starts_with('[') && json.contains("ignore-previous-rules"));
        assert_eq!((prepared.rules(), prepared.dropped()), (2, 1));
    }

    #[test]
    fn a_list_with_nothing_to_block_has_no_parts() {
        let prepared = prepare("easylist", b"! only a comment\nexample.com##.ad\n");
        assert!(prepared.parts.is_empty());
        assert_eq!(prepared.rules(), 0);
    }

    #[test]
    fn invalid_utf8_is_read_lossily() {
        let prepared = prepare("easylist", b"||ads.example^\n||caf\xe9.example^\n");
        assert_eq!(prepared.parts.len(), 1);
    }

    #[test]
    fn known_lists_are_found_by_id() {
        assert_eq!(spec("easylist"), Some(&EASYLIST));
        assert_eq!(spec("nope"), None);
        assert!(EASYLIST.url.starts_with("https://"));
    }
}
