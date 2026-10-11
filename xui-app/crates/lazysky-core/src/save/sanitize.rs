//! File and folder names made from what a server or a user typed.

use std::path::{Path, PathBuf};

/// The longest a name part may be, in bytes.
pub const MAX_NAME: usize = 100;
const FALLBACK: &str = "picture";

/// Keeps `[A-Za-z0-9._-]`, replaces everything else with `_`, drops leading
/// dots, truncates to [`MAX_NAME`] bytes. Never empty.
pub fn sanitize(name: &str) -> String {
    let mapped: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = mapped.trim_start_matches('.');
    // ASCII only after the mapping, so any byte count is a char boundary.
    let cut = &trimmed[..trimmed.len().min(MAX_NAME)];
    if cut.is_empty() || cut.chars().all(|c| c == '_') {
        FALLBACK.to_string()
    } else {
        cut.to_string()
    }
}

/// The extension a picture URL implies: `.../cid@jpeg` is `jpg`, `@png`,
/// `@webp`, `@gif`; anything else is `jpg`.
pub fn extension_of(url: &str) -> &'static str {
    let tail = url
        .rsplit('@')
        .next()
        .filter(|_| url.contains('@'))
        .unwrap_or("");
    let tail = tail
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match tail.as_str() {
        "png" => "png",
        "webp" => "webp",
        "gif" => "gif",
        _ => "jpg",
    }
}

/// `<handle>_<rkey>_<n>.<ext>` for the `n`th picture of a post.
pub fn file_name_for(handle: &str, rkey: &str, n: usize, url: &str) -> String {
    sanitize(&format!("{handle}_{rkey}_{n}.{}", extension_of(url)))
}

/// `<pictures>/LazySky/<sanitized handle>`.
pub fn folder_for(pictures: &Path, handle: &str) -> PathBuf {
    pictures.join("LazySky").join(sanitize(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_safe_characters_survive() {
        assert_eq!(sanitize("alice.bsky.social"), "alice.bsky.social");
        assert_eq!(sanitize("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize("C:\\Windows\\evil.exe"), "C__Windows_evil.exe");
        assert_eq!(sanitize(".hidden"), "hidden");
        assert_eq!(sanitize("a\nb:c?.png"), "a_b_c_.png");
        assert_eq!(sanitize("\u{e9}\u{e9}"), "picture");
        assert_eq!(sanitize(""), "picture");
        assert_eq!(sanitize("..."), "picture");
        assert_eq!(sanitize(&"x".repeat(300)).len(), MAX_NAME);
        assert_eq!(sanitize("two words.jpg"), "two_words.jpg");
    }

    #[test]
    fn a_sanitised_name_is_never_a_path() {
        for hostile in ["/", "\\", "..", "a/../b", "CON:", "x\0y", "~", "%2e%2e"] {
            let s = sanitize(hostile);
            assert!(
                !s.contains('/') && !s.contains('\\') && !s.starts_with('.'),
                "{s}"
            );
            assert!(Path::new(&s).components().count() == 1, "{s}");
        }
    }

    #[test]
    fn extensions_come_from_the_url_suffix() {
        let base = "https://cdn.bsky.app/img/feed_fullsize/plain/did:plc:x/bafkreiabc";
        assert_eq!(extension_of(&format!("{base}@jpeg")), "jpg");
        assert_eq!(extension_of(&format!("{base}@png")), "png");
        assert_eq!(extension_of(&format!("{base}@webp")), "webp");
        assert_eq!(extension_of(&format!("{base}@GIF")), "gif");
        assert_eq!(extension_of(&format!("{base}@jpeg?x=1")), "jpg");
        assert_eq!(extension_of(base), "jpg");
        assert_eq!(
            extension_of("https://h.test/a.png"),
            "jpg",
            "no @ means jpg"
        );
    }

    #[test]
    fn the_suggested_name_has_handle_rkey_and_index() {
        assert_eq!(
            file_name_for("alice.test", "3kabc", 2, "https://c.test/x@png"),
            "alice.test_3kabc_2.png"
        );
        assert_eq!(
            folder_for(Path::new("/p"), "al/ice"),
            Path::new("/p").join("LazySky").join("al_ice")
        );
    }
}
