//! The small grammars of manifest fields: system names, MIME types, interface
//! names and topic patterns (`docs/packages.md`, "Field grammar"). File rules
//! have their own module, [`crate::files`].

use alloc::vec::Vec;

/// Longest `system_name`, in bytes.
const MAX_SYSTEM_NAME: usize = 128;

/// `[a-z0-9]` labels joined by `.`, at least three, none starting or ending in
/// `-`, at most 128 bytes.
pub(crate) fn valid_system_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_SYSTEM_NAME {
        return false;
    }
    let mut labels = 0;
    for label in name.split('.') {
        labels += 1;
        if label.is_empty() || label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return false;
        }
    }
    labels >= 3
}

/// `type/subtype` over `[a-z0-9.+-]`.
pub(crate) fn valid_mime_type(mime: &str) -> bool {
    let mut parts = mime.split('/');
    let (Some(kind), Some(subtype), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && kind.bytes().all(is_mime_byte)
        && subtype.bytes().all(is_mime_byte)
}

fn is_mime_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-')
}

/// `[a-z0-9]+(\.[a-z0-9]+)*\.v[0-9]+`.
pub(crate) fn valid_interface(interface: &str) -> bool {
    let mut parts: Vec<&str> = interface.split('.').collect();
    let Some(version) = parts.pop() else {
        return false;
    };
    if parts.is_empty() {
        return false;
    }
    if !version.starts_with('v') || version.len() < 2 {
        return false;
    }
    if !version[1..].bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    parts.iter().all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

/// `publish:`/`subscribe:` then `/`-separated segments of `[a-z0-9_.-]+`, `+`,
/// or a final `#`. A segment of dots only (`.`, `..`) is refused, as the kernel
/// refuses it (`kernel/src/ipc/topics.rs`).
pub(crate) fn valid_topic(topic: &str) -> bool {
    let rest = topic
        .strip_prefix("publish:")
        .or_else(|| topic.strip_prefix("subscribe:"));
    let Some(rest) = rest else {
        return false;
    };
    if rest.is_empty() {
        return false;
    }
    let segments: Vec<&str> = rest.split('/').collect();
    let last = segments.len() - 1;
    segments.iter().enumerate().all(|(index, segment)| {
        if segment.is_empty() {
            return false;
        }
        if *segment == "#" {
            return index == last;
        }
        if *segment == "+" {
            return true;
        }
        if segment.bytes().all(|b| b == b'.') {
            return false;
        }
        segment.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        })
    })
}
