//! Paths and names between LazyOS and an SMB share.
//!
//! A path the caller gives (`dir/file`, relative to the share root) becomes
//! the backslash form SMB2 sends; `..`, stream names (`:`), wildcards and
//! control characters are refused rather than passed to the server. A name
//! the server lists comes back only if it is one plain component.

use alloc::string::String;
use alloc::vec::Vec;

use crate::crypto::utf16le;
use crate::Error;

/// Longest component, characters (NTFS and Samba's limit).
pub const MAX_COMPONENT: usize = 255;
/// Longest path, characters.
pub const MAX_PATH: usize = 1024;

/// Whether `c` may appear in a component the client sends or lists. Besides
/// separators, wildcards and controls, the invisible formatting characters
/// are refused (zero-width ones, bidirectional overrides and isolates, the
/// byte-order mark): a listed name carrying them could make a terminal show a
/// different name than the one on the server.
fn allowed(c: char) -> bool {
    !(c.is_control()
        || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        || c == '\u{FEFF}')
}

/// One component: not empty, not `.`/`..`, no forbidden characters.
pub fn component_ok(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.chars().count() <= MAX_COMPONENT
        && name.chars().all(allowed)
}

/// `path` (slash-separated, relative to the share root; a leading slash and
/// `.` components are dropped) as the SMB2 backslash form, UTF-16LE. The share
/// root is the empty path.
pub fn to_smb(path: &str) -> Result<Vec<u8>, Error> {
    let mut joined = String::new();
    for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if !component_ok(part) {
            return Err(Error::BadName);
        }
        if !joined.is_empty() {
            joined.push('\\');
        }
        joined.push_str(part);
    }
    if joined.chars().count() > MAX_PATH {
        return Err(Error::BadName);
    }
    Ok(utf16le(&joined))
}

/// A share name: one component, as in `\\server\share`. `$` is allowed
/// (administrative shares).
pub fn share_ok(share: &str) -> bool {
    component_ok(share) && share.chars().count() <= 80
}

/// The UNC path of a share for TREE_CONNECT.
pub fn unc(server: &str, share: &str) -> Result<Vec<u8>, Error> {
    let server_ok = !server.is_empty()
        && server.len() <= 255
        && server
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !server_ok || !share_ok(share) {
        return Err(Error::BadName);
    }
    let mut text = String::from("\\\\");
    text.push_str(server);
    text.push('\\');
    text.push_str(share);
    Ok(utf16le(&text))
}

/// A name from a directory listing, if it is one plain component the client
/// would itself accept; `.`, `..` and anything else come back as `None`.
pub fn listed(bytes: &[u8]) -> Option<String> {
    let name = crate::ntlm::from_utf16le(bytes)?;
    // A listed name may legitimately carry characters the client refuses to
    // send (`:`, `?`); only those that could change a path's meaning here are
    // fatal. A component with them cannot be opened, so it is skipped.
    component_ok(&name).then_some(name)
}
