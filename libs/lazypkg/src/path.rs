//! Entry-name safety.
//!
//! An archive is untrusted input, so a name is rejected before it can reach a
//! filesystem or a security label. A name is a `/`-separated relative path of
//! UTF-8 text: no backslash, no drive letter, no `..`/`.` component, no leading
//! slash, no control byte, no NUL. A single trailing `/` marks a directory and
//! is allowed; the directory must then carry no data (checked by the zip
//! parser). Names are also bounded to [`crate::MAX_NAME_LEN`] bytes.

use crate::MAX_NAME_LEN;

/// Validate one entry name. Returns a one-line reason on rejection, suitable
/// for [`crate::OpenError::BadPath`].
pub(crate) fn validate(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("is empty");
    }
    if name.len() > MAX_NAME_LEN {
        return Err("is longer than 255 bytes");
    }
    if name.contains('\0') {
        return Err("contains a NUL byte");
    }
    if name.chars().any(char::is_control) {
        return Err("contains a control character");
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Err("is absolute");
    }
    if name.contains('\\') {
        return Err("contains a backslash");
    }
    if has_drive_letter(name) {
        return Err("has a drive letter");
    }

    // A trailing `/` only marks a directory; it is not a path component.
    let path = name.strip_suffix('/').unwrap_or(name);
    if path.is_empty() {
        return Err("is empty");
    }
    for component in path.split('/') {
        if component.is_empty() {
            return Err("has an empty path component");
        }
        if component == "." || component == ".." {
            return Err("contains a `.` or `..` component");
        }
    }
    Ok(())
}

/// `C:` style prefixes would be interpreted as absolute by some filesystems.
fn has_drive_letter(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_relative_paths() {
        for name in [
            "manifest.toml",
            "bin/paint.elf",
            "icons/app-16.png",
            "resources/themes/dark/theme.json",
            "docs/README.md",
            "bin/",
        ] {
            assert_eq!(validate(name), Ok(()), "should accept {name:?}");
        }
    }

    #[test]
    fn rejects_escapes_and_absolute_names() {
        for (name, reason) in [
            ("bin/../x", "contains a `.` or `..` component"),
            ("..", "contains a `.` or `..` component"),
            (".", "contains a `.` or `..` component"),
            ("/etc/passwd", "is absolute"),
            ("\\windows\\system32", "is absolute"),
            ("C:/Windows", "has a drive letter"),
            ("a/b\\c", "contains a backslash"),
            ("a//b", "has an empty path component"),
            ("a\0b", "contains a NUL byte"),
            ("a\tb", "contains a control character"),
            ("", "is empty"),
            ("/", "is absolute"),
        ] {
            assert_eq!(validate(name), Err(reason), "should reject {name:?}");
        }
    }

    #[test]
    fn rejects_overlong_names() {
        let long = "a".repeat(MAX_NAME_LEN + 1);
        assert_eq!(validate(&long), Err("is longer than 255 bytes"));
        assert_eq!(validate(&"a".repeat(MAX_NAME_LEN)), Ok(()));
    }
}
