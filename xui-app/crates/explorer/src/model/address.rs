#![forbid(unsafe_code)]

//! The address bar's text, resolved to a path: what the user typed, against
//! the window's folder and the home directory, with `.` and `..` folded
//! lexically (the platform is never asked).

use std::path::{Component, Path, PathBuf};

/// Resolves `text` typed into the address bar of a window showing `current`:
/// surrounding blanks are ignored, `~` (alone or as `~/...`) is `home`, a
/// relative path is relative to `current`, and `.`/`..` components are
/// folded without touching the disk (`..` at a root stays at the root).
///
/// Returns `None` for blank text, or for `~` when there is no home.
pub fn resolve_address(text: &str, current: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let joined = if text == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home?.join(rest)
    } else {
        current.join(text)
    };
    Some(normalize(&joined))
}

/// Folds `.` and `..` lexically. A `..` with nothing left to pop above a
/// root is dropped.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.file_name().is_some() {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::resolve_address;

    fn at(text: &str) -> Option<PathBuf> {
        resolve_address(
            text,
            Path::new("/home/user/docs"),
            Some(Path::new("/home/user")),
        )
    }

    #[test]
    fn an_absolute_path_is_taken_as_typed() {
        assert_eq!(at("/system/bin"), Some(PathBuf::from("/system/bin")));
        assert_eq!(
            at("  /tmp  "),
            Some(PathBuf::from("/tmp")),
            "blanks trimmed"
        );
        assert_eq!(at("/"), Some(PathBuf::from("/")));
    }

    #[test]
    fn a_relative_path_is_relative_to_the_window() {
        assert_eq!(at("notes"), Some(PathBuf::from("/home/user/docs/notes")));
        assert_eq!(at(".."), Some(PathBuf::from("/home/user")));
        assert_eq!(
            at("../pics/./2024"),
            Some(PathBuf::from("/home/user/pics/2024"))
        );
    }

    #[test]
    fn dot_dot_stops_at_the_root() {
        assert_eq!(at("/../../tmp"), Some(PathBuf::from("/tmp")));
        assert_eq!(at("/.."), Some(PathBuf::from("/")));
    }

    #[test]
    fn tilde_is_home() {
        assert_eq!(at("~"), Some(PathBuf::from("/home/user")));
        assert_eq!(at("~/music"), Some(PathBuf::from("/home/user/music")));
        assert_eq!(resolve_address("~", Path::new("/"), None), None, "no home");
    }

    #[test]
    fn blank_text_resolves_to_nothing() {
        assert_eq!(at(""), None);
        assert_eq!(at("   "), None);
    }
}
