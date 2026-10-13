//! The small grammars of manifest fields: system names, MIME types, interface
//! names and topic patterns (`docs/packages.md`, "Field grammar"). File rules
//! have their own module, [`crate::files`].

use alloc::vec::Vec;

/// Longest `system_name`, in bytes.
const MAX_SYSTEM_NAME: usize = 128;
/// Longest `[processes.<name>]` name, in bytes. Chosen so the child label
/// `app:<system_name>/<name>` (4 + 128 + 1 + 24 bytes at most) always fits the
/// kernel's 160-byte label (`kernel/src/ipc/labels.rs`).
pub const MAX_PROCESS_NAME: usize = 24;
/// Longest `[processes.<name>] program`, in bytes (`spawnv`'s `PATH_MAX`).
pub const MAX_PROGRAM: usize = 255;

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

/// A `[processes.<name>]` name: `[a-z0-9]` first, then `[a-z0-9_-]`, at most
/// [`MAX_PROCESS_NAME`] bytes. It becomes the part after the `/` of the
/// kernel label, whose own grammar (`labels::parse`) is the same.
pub(crate) fn valid_process_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_PROCESS_NAME
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

/// A `[processes.<name>] program`: an absolute path of `/`-separated segments
/// of `[A-Za-z0-9_.-]`, none empty, `.` or `..`, at most [`MAX_PROGRAM`]
/// bytes. The kernel compares it, byte for byte, with the path `execve`
/// resolved, so it must already be in that canonical form.
pub(crate) fn valid_program_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    path.len() <= MAX_PROGRAM
        && rest.split('/').all(|segment| {
            !segment.is_empty()
                && !segment.bytes().all(|b| b == b'.')
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_names() {
        for good in ["webprocess", "networkprocess", "a", "x_1-y", "0"] {
            assert!(valid_process_name(good), "{good}");
        }
        for bad in [
            "",
            "Web",
            "-web",
            "_web",
            "we b",
            "web/x",
            "web.x",
            "web:x",
            "aaaaaaaaaaaaaaaaaaaaaaaaa", // 25 bytes
        ] {
            assert!(!valid_process_name(bad), "{bad:?}");
        }
        assert!(valid_process_name("aaaaaaaaaaaaaaaaaaaaaaaa")); // 24 bytes
    }

    #[test]
    fn program_paths() {
        for good in [
            "/system/bin/webprocess",
            "/a",
            "/apps/org.lazy.x/1.0-abcd/bin/helper.elf",
        ] {
            assert!(valid_program_path(good), "{good}");
        }
        for bad in [
            "",
            "/",
            "relative/x",
            "bin/x",
            "/a//b",
            "/a/./b",
            "/a/../b",
            "/a/",
            "/a/b c",
            "/a/$HOME",
            "/..",
            "/caf\u{e9}",
        ] {
            assert!(!valid_program_path(bad), "{bad:?}");
        }
        let long = alloc::format!("/{}", "a".repeat(MAX_PROGRAM));
        assert!(!valid_program_path(&long));
        assert!(valid_program_path(&long[..MAX_PROGRAM]));
    }
}
