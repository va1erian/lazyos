//! Where an app's file pickers start.
//!
//! The Installer's package picker and LazyWriter's Open/Save dialogs share one
//! rule: the user's `$HOME` when it is an absolute, existing directory, else
//! `/transient` (a writable ramfs every boot has, and the other place `pkgd`
//! installs from). `$HOME` comes from the environment `init` hands the app, so
//! it is checked rather than trusted.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The folder a picker opens in when the app has no better one.
pub fn default_dir() -> PathBuf {
    resolve_default_dir(std::env::var_os("HOME"), |path| path.is_dir())
}

/// [`default_dir`] over an explicit `$HOME` and directory test, so the rule is
/// testable without touching the process environment or the filesystem.
pub fn resolve_default_dir(home: Option<OsString>, is_dir: impl Fn(&Path) -> bool) -> PathBuf {
    home.map(PathBuf::from)
        .filter(|home| home.is_absolute() && is_dir(home))
        .unwrap_or_else(|| PathBuf::from(fhs::mount::TRANSIENT))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(value: &str) -> Option<OsString> {
        Some(OsString::from(value))
    }

    #[test]
    fn an_existing_absolute_home_is_used() {
        let dir = resolve_default_dir(home("/home/alice"), |_| true);
        assert_eq!(dir, PathBuf::from("/home/alice"));
    }

    #[test]
    fn a_missing_home_falls_back_to_transient() {
        let dir = resolve_default_dir(None, |_| true);
        assert_eq!(dir, PathBuf::from(fhs::mount::TRANSIENT));
    }

    #[test]
    fn a_relative_home_falls_back_to_transient() {
        let dir = resolve_default_dir(home("alice"), |_| true);
        assert_eq!(dir, PathBuf::from(fhs::mount::TRANSIENT));
        let dir = resolve_default_dir(home(""), |_| true);
        assert_eq!(dir, PathBuf::from(fhs::mount::TRANSIENT));
    }

    #[test]
    fn a_home_that_is_not_a_directory_falls_back_to_transient() {
        let dir = resolve_default_dir(home("/home/gone"), |_| false);
        assert_eq!(dir, PathBuf::from(fhs::mount::TRANSIENT));
    }

    #[test]
    fn the_directory_test_sees_the_home_path() {
        let dir = resolve_default_dir(home("/home/bob"), |path| path == Path::new("/home/bob"));
        assert_eq!(dir, PathBuf::from("/home/bob"));
    }
}
