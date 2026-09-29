//! `mimed`'s MIME database: the built-in extension/filename table plus a
//! `/etc/mime.types`-style override read through the native file API at boot.
//!
//! Split out of `mimed.rs`, which is past the file-size budget.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use user::messenger::mime;
use user::sys;

use super::validate::{valid_extension, valid_mime};

/// Boot-time MIME database: extension (lowercase, no dot) to type.
const BUILTIN_TYPES: &[(&str, &str)] = &[
    ("txt", "text/plain"),
    ("text", "text/plain"),
    ("log", "text/plain"),
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("rs", "text/x-rust"),
    ("elf", "application/x-elf"),
    ("png", "image/png"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("json", "application/json"),
    ("sh", "text/x-shellscript"),
    ("toml", "application/toml"),
    ("c", "text/x-c"),
    ("h", "text/x-c"),
];

/// Boot-time MIME database: exact filename to type (checked before the
/// extension, so `Makefile` is not `application/octet-stream`).
const BUILTIN_NAMES: &[(&str, &str)] = &[
    ("Makefile", "text/x-makefile"),
    ("README", "text/plain"),
    ("LICENSE", "text/plain"),
];

/// Override files tried in order at boot. The first is the conventional
/// `/etc/mime.types` path (an ext2 volume can carry it); the second is the
/// 8.3-safe name the FAT boot image ships, because the FAT reader only
/// resolves short names in the root directory.
const OVERRIDE_PATHS: &[&str] = &["/etc/mime.types", "MIME.TYP"];

/// Largest override file read at boot.
pub(crate) const OVERRIDE_BUFFER: usize = 4096;

/// The MIME database: built-in and override entries. A lookup checks the
/// exact filename first, then the override extensions, then the built-in
/// extensions, and falls back to `application/octet-stream`.
pub(crate) struct MimeDb {
    names: Vec<(String, String)>,
    builtin: Vec<(String, String)>,
    overrides: Vec<(String, String)>,
    /// Path the override table was read from, when one loaded.
    pub(crate) source: Option<String>,
}

impl MimeDb {
    /// The built-in table.
    pub(crate) fn builtin() -> MimeDb {
        MimeDb {
            names: BUILTIN_NAMES
                .iter()
                .map(|(name, mime)| (name.to_string(), mime.to_string()))
                .collect(),
            builtin: BUILTIN_TYPES
                .iter()
                .map(|(extension, mime)| (extension.to_string(), mime.to_string()))
                .collect(),
            overrides: Vec::new(),
            source: None,
        }
    }

    /// Apply a `/etc/mime.types`-style override table and remember the
    /// source; `false` when every candidate path is missing.
    pub(crate) fn load_overrides(&mut self, buffer: &mut [u8]) -> bool {
        for path in OVERRIDE_PATHS {
            let Some(length) = read_override(path, buffer) else {
                continue;
            };
            let text = core::str::from_utf8(&buffer[..length]).unwrap_or("");
            self.parse_overrides(text);
            self.source = Some(String::from(*path));
            return true;
        }
        false
    }

    /// Parse `<mime> <ext>...` lines; `#` starts a comment.
    fn parse_overrides(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut tokens = line.split_whitespace();
            let Some(mime_type) = tokens.next() else {
                continue;
            };
            if !valid_mime(mime_type) {
                continue;
            }
            for extension in tokens {
                if valid_extension(extension) {
                    self.overrides
                        .push((extension.to_ascii_lowercase(), mime_type.to_string()));
                }
            }
        }
    }

    /// The MIME type for a path.
    pub(crate) fn guess(&self, path: &str) -> String {
        let name = file_name(path);
        if let Some(mime_type) = lookup(&self.names, name) {
            return mime_type.to_string();
        }
        if let Some(extension) = extension(name) {
            if let Some(mime_type) = lookup(&self.overrides, extension) {
                return mime_type.to_string();
            }
            if let Some(mime_type) = lookup(&self.builtin, extension) {
                return mime_type.to_string();
            }
        }
        String::from(mime::FALLBACK_MIME)
    }

    /// Number of override entries loaded (boot diagnostics).
    pub(crate) fn override_count(&self) -> usize {
        self.overrides.len()
    }
}

/// The last path component (`/` and `\` both separate, so a Linux-style path
/// works on the console too).
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The extension after the last dot of a file name, if it has one.
fn extension(name: &str) -> Option<&str> {
    let (stem, extension) = name.rsplit_once('.')?;
    if stem.is_empty() || extension.is_empty() {
        return None;
    }
    Some(extension)
}

/// The last (case-insensitive) `key` match in a table, so an override
/// appended later replaces a built-in entry.
fn lookup<'a>(table: &'a [(String, String)], key: &str) -> Option<&'a str> {
    table
        .iter()
        .rev()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, mime_type)| mime_type.as_str())
}

/// Read `path` through the native file API (the VFS, permission-checked);
/// `None` when the file is missing or the path is too long.
fn read_override(path: &str, buffer: &mut [u8]) -> Option<usize> {
    let mut name = [0u8; 64];
    if path.len() + 1 > name.len() {
        return None;
    }
    name[..path.len()].copy_from_slice(path.as_bytes());
    sys::read_file(&name[..path.len() + 1], buffer)
}
