//! One archive member as every format reports it, and entry-path
//! normalisation.
//!
//! A name in an archive is whatever its writer put there: absolute paths,
//! Windows separators, `.` and `..` components, empty components, control
//! characters. [`normalize`] turns it into `/`-separated components with the
//! empty and `.` ones dropped, and records whether anything dangerous (a `..`,
//! a leading `/`, a drive letter) was present: such an entry is listed, but
//! [`crate::safety`] never extracts it.

/// What an entry is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link to `target` (as stored, not normalised).
    Symlink { target: String },
    /// A tar hard link to another member, `target` (normalised).
    Hardlink { target: String },
    /// A device, FIFO or other special file: listed, never extracted.
    Special,
}

impl EntryKind {
    /// Whether this is a directory.
    pub fn is_dir(&self) -> bool {
        matches!(self, EntryKind::Dir)
    }
}

/// One member of an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Position in the archive's listing; [`Archive::visit`](crate::Archive::visit)
    /// reports entries by it.
    pub index: usize,
    /// The normalised path: `/`-separated, no leading or trailing `/`, no
    /// empty or `.` components. Never empty (an entry whose name normalises to
    /// nothing is named `_`).
    pub path: String,
    /// Set when the stored name was absolute or contained `..`: listed, never
    /// extracted.
    pub unsafe_path: bool,
    /// What it is.
    pub kind: EntryKind,
    /// Uncompressed size in bytes (0 for directories and links).
    pub size: u64,
    /// Compressed size, when the format records one per entry.
    pub packed: Option<u64>,
    /// Modification time, seconds since the Unix epoch (UTC).
    pub modified: Option<i64>,
    /// Unix permission bits (`0o7777` at most), when recorded.
    pub mode: Option<u32>,
    /// The compression method's display name (`Deflate`, `Store`, `LZMA2`).
    pub method: String,
    /// Whether the entry's data is encrypted (listed, not extracted).
    pub encrypted: bool,
    /// The CRC-32 the archive records for the data, when it records one.
    pub crc: Option<u32>,
}

impl Entry {
    /// A file entry at `path` (normalised here) with everything else unknown;
    /// the format readers fill the rest in.
    pub fn new(index: usize, raw_path: &str, kind: EntryKind) -> Entry {
        let (path, unsafe_path) = normalize(raw_path);
        Entry {
            index,
            path,
            unsafe_path,
            kind,
            size: 0,
            packed: None,
            modified: None,
            mode: None,
            method: String::new(),
            encrypted: false,
            crc: None,
        }
    }

    /// The last path component.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// Whether this entry is `folder` itself or lies below it (`folder` is a
    /// normalised path; `""` is the root and contains everything).
    pub fn is_under(&self, folder: &str) -> bool {
        is_under(&self.path, folder)
    }
}

/// Whether normalised `path` is `folder` or lies below it.
pub fn is_under(path: &str, folder: &str) -> bool {
    folder.is_empty()
        || path == folder
        || (path.len() > folder.len()
            && path.starts_with(folder)
            && path.as_bytes()[folder.len()] == b'/')
}

/// Normalise a stored entry name; returns the path and whether the name was
/// unsafe to extract (absolute, a drive letter, or a `..` component).
pub fn normalize(raw: &str) -> (String, bool) {
    let unified = raw.replace('\\', "/");
    let mut unsafe_path = unified.starts_with('/') || has_drive_prefix(&unified);
    let mut parts: Vec<String> = Vec::new();
    for component in unified.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                unsafe_path = true;
                parts.push("..".to_owned());
            }
            other => parts.push(clean_component(other)),
        }
    }
    if parts.is_empty() {
        parts.push("_".to_owned());
    }
    (parts.join("/"), unsafe_path)
}

/// `C:` or `c:` at the start of a name.
fn has_drive_prefix(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// A component with control characters replaced, so a name cannot move the
/// cursor or hide itself in a list (NUL can never reach a path either).
fn clean_component(component: &str) -> String {
    component
        .chars()
        .map(|c| if c.is_control() { '_' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_and_dots_are_normalised() {
        assert_eq!(normalize("a/./b//c/"), ("a/b/c".to_owned(), false));
        assert_eq!(
            normalize("dir\\sub\\f.txt"),
            ("dir/sub/f.txt".to_owned(), false)
        );
    }

    #[test]
    fn traversal_and_absolute_names_are_flagged() {
        assert!(normalize("../etc/passwd").1);
        assert!(normalize("a/../../b").1);
        assert!(normalize("/etc/passwd").1);
        assert!(normalize("C:\\Windows\\x").1);
        assert!(!normalize("a..b/c").1);
    }

    #[test]
    fn an_empty_name_still_has_a_component() {
        assert_eq!(normalize("/").0, "_");
        assert_eq!(normalize("").0, "_");
    }

    #[test]
    fn control_characters_are_replaced() {
        assert_eq!(normalize("a\u{1b}[2Jb").0, "a_[2Jb");
    }

    #[test]
    fn containment_respects_component_boundaries() {
        assert!(is_under("docs/a.txt", "docs"));
        assert!(is_under("docs", "docs"));
        assert!(!is_under("docsx/a.txt", "docs"));
        assert!(is_under("anything", ""));
    }
}
