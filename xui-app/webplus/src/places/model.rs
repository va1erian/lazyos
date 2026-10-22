//! What a bookmark is, the three roots, the caps, and the rules for what text may be kept.

use std::fmt;

use crate::sanitize;

/// An entry's number. Unique for the life of the file, never reused after a removal.
pub type Id = u64;

/// The "Bookmarks bar" root: its entries are the ones shown first in the Bookmarks menu.
pub const BAR: Id = 1;
/// The "Other bookmarks" root.
pub const OTHER: Id = 2;
/// The "Unsorted" root, where a one-click bookmark goes.
pub const UNSORTED: Id = 3;
/// The first number an entry of the person's gets.
pub const FIRST_ID: Id = 4;
/// The parent number of a root (they hang from nothing).
pub const NO_PARENT: Id = 0;

/// The roots, in the order they are shown.
pub const ROOTS: [Id; 3] = [BAR, OTHER, UNSORTED];

/// The longest title kept, in characters.
pub const MAX_TITLE: usize = 512;
/// The longest address kept, in bytes.
pub const MAX_URL: usize = 2048;
/// How many entries (not counting the roots) a store holds.
pub const MAX_ENTRIES: usize = 100_000;
/// How deep folders nest. A bound that keeps every walk up or down the tree short, whatever a
/// hand-edited file says.
pub const MAX_DEPTH: usize = 16;

/// Whether `id` is one of the fixed roots.
pub fn is_root(id: Id) -> bool {
    ROOTS.contains(&id)
}

/// The title of a root.
pub fn root_title(id: Id) -> Option<&'static str> {
    match id {
        BAR => Some("Bookmarks bar"),
        OTHER => Some("Other bookmarks"),
        UNSORTED => Some("Unsorted"),
        _ => None,
    }
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Link,
    Separator,
}

impl Kind {
    pub(super) fn code(self) -> u8 {
        match self {
            Kind::Folder => 1,
            Kind::Link => 2,
            Kind::Separator => 3,
        }
    }

    pub(super) fn from_code(code: u8) -> Option<Kind> {
        match code {
            1 => Some(Kind::Folder),
            2 => Some(Kind::Link),
            3 => Some(Kind::Separator),
            _ => None,
        }
    }
}

/// One bookmark, folder or separator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: Id,
    /// A root or a folder.
    pub parent: Id,
    pub kind: Kind,
    /// Empty for a separator.
    pub title: String,
    /// Only a link has one.
    pub url: String,
    /// The place among the entries of the same parent, smallest first.
    pub position: u32,
    /// Seconds since the Unix epoch.
    pub created: u64,
    pub modified: u64,
}

/// Why a store refused an operation.
#[derive(Debug)]
pub enum Error {
    /// A `javascript:` address (a bookmark of it would run a script in whatever page it is opened
    /// from), or an empty one.
    Unsupported,
    /// An address longer than [`MAX_URL`].
    UrlTooLong,
    /// [`MAX_ENTRIES`] entries are kept already.
    Full,
    /// No such entry (or a root, which cannot be changed).
    NoSuchEntry,
    /// The parent is not a root or a folder, is the entry itself or inside it, or is nested too
    /// deep.
    BadParent,
    /// The change is made in memory but could not be written.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported => f.write_str("this address cannot be bookmarked"),
            Error::UrlTooLong => f.write_str("the address is too long"),
            Error::Full => f.write_str("there are too many bookmarks"),
            Error::NoSuchEntry => f.write_str("there is no such bookmark"),
            Error::BadParent => f.write_str("it cannot go in that folder"),
            Error::Io(e) => write!(f, "not saved: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

/// A title fit to keep: one line, no control or reordering characters, at most [`MAX_TITLE`]
/// characters.
pub fn clean_title(title: &str) -> String {
    sanitize::line(title, MAX_TITLE)
}

/// An address fit to keep: control characters removed (a browser ignores them inside an address,
/// so `java\tscript:` is still a script), edges trimmed. Refused when empty, over [`MAX_URL`]
/// bytes, or `javascript:`.
pub fn clean_url(url: &str) -> Result<String, Error> {
    let url: String = url.chars().filter(|c| !sanitize::is_unsafe(*c)).collect();
    let url = url.trim();
    if url.is_empty() {
        return Err(Error::Unsupported);
    }
    if url.len() > MAX_URL {
        return Err(Error::UrlTooLong);
    }
    // Whitespace inside the scheme is skipped by a browser's parser as well.
    let probe: String = url
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(11)
        .collect::<String>()
        .to_ascii_lowercase();
    if probe.starts_with("javascript:") {
        return Err(Error::Unsupported);
    }
    Ok(url.to_owned())
}
