//! `text/uri-list` (RFC 2483), the payload a drag of files carries between
//! apps: one `file://` URI per line, CRLF-separated, with every byte outside
//! the unreserved set percent-encoded so any file name survives.
//!
//! Decoding is strict about what it accepts, since the bytes come from
//! another app: only absolute local `file:` URIs (an empty or `localhost`
//! host), no NUL, a bounded number of paths; comment lines are skipped.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// The MIME type.
pub const MIME: &str = "text/uri-list";
/// Most paths a decoded list yields.
pub const MAX_PATHS: usize = 1024;

/// Whether `byte` is RFC 3986 unreserved or a path separator.
fn plain(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/')
}

/// One path as a `file://` URI.
pub fn uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if plain(byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A list of absolute paths as a uri-list.
pub fn encode(paths: &[PathBuf]) -> String {
    paths.iter().map(|path| uri(path) + "\r\n").collect()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The local path one `file:` URI names, or `None`.
pub fn path_of(line: &str) -> Option<PathBuf> {
    let rest = line.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let high = hex(*bytes.get(i + 1)?)?;
            let low = hex(*bytes.get(i + 2)?)?;
            out.push(high << 4 | low);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    if out.contains(&0) {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(out)))
}

/// The local paths of a uri-list payload (invalid lines are dropped).
pub fn decode(bytes: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(path_of)
        .take(MAX_PATHS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_name_round_trips() {
        let paths = vec![
            PathBuf::from("/home/alice/My Files/r\u{e9}sum\u{e9} (1).txt"),
            PathBuf::from("/tmp/100%/a#b?c"),
        ];
        let encoded = encode(&paths);
        assert!(
            encoded.starts_with("file:///home/alice/My%20Files/r%C3%A9sum%C3%A9%20%281%29.txt\r\n")
        );
        assert_eq!(decode(encoded.as_bytes()), paths);
    }

    #[test]
    fn only_absolute_local_files_are_accepted() {
        let list = b"# a comment\r\nfile://localhost/etc/motd\r\nhttp://example.com/x\r\nfile://host/remote\r\nfile:///a%00b\r\nfile:///ok\r\n";
        assert_eq!(
            decode(list),
            [PathBuf::from("/etc/motd"), PathBuf::from("/ok")]
        );
    }

    #[test]
    fn a_broken_escape_drops_the_line() {
        assert_eq!(path_of("file:///a%4"), None);
        assert_eq!(path_of("file:///a%zz"), None);
    }
}
