//! Validating a file path passed on the command line.
//!
//! A desktop app's `argv` comes from `init`'s `Launch`, which takes the path
//! from the open-with registry or another app: it is **untrusted input**. A
//! path this module accepts is absolute, contains no NUL, and is shorter than
//! [`MAX_PATH_BYTES`]; anything else is rejected rather than handed to the
//! filesystem.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The longest accepted path, in bytes.
///
/// LazyOS paths are far shorter, but bounding the value keeps a hostile
/// argument from forcing a huge allocation or an unbounded string copy into a
/// `Launch`/`mimed` request.
pub const MAX_PATH_BYTES: usize = 4096;

/// Validates `arg` as a file argument, returning the path to open.
///
/// `None` means "no usable argument": the caller should fall back to its own
/// default. Rejecting rather than falling back on a *present but bad* argument
/// would be surprising for a user, but a bad argument is also not something to
/// silently open elsewhere; the caller decides (the apps treat `None` as "use
/// the home/default path").
pub fn path_arg(arg: Option<OsString>) -> Option<PathBuf> {
    let arg = arg?;
    let path = PathBuf::from(arg);
    if !is_acceptable(&path) {
        return None;
    }
    Some(path)
}

/// The file argument in a full `argv`.
///
/// `init`'s launch line is `<app> [--client] <path> attempt=N`, so the program
/// name and any `-flag` or `attempt=` token are skipped and the first remaining
/// token is validated as a path. A path with spaces is a known limitation of
/// the kernel's space-split launch line (see
/// `docs/xui-apps-migration-status.md`).
pub fn file_arg<I: IntoIterator<Item = OsString>>(args: I) -> Option<PathBuf> {
    args.into_iter()
        .skip(1)
        .find(|arg| {
            let text = arg.to_string_lossy();
            !text.starts_with('-') && !text.starts_with("attempt=")
        })
        .and_then(|arg| path_arg(Some(arg)))
}

/// Whether `path` may be opened from an argument: absolute, non-empty, no NUL,
/// and bounded.
pub fn is_acceptable(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    !bytes.is_empty() && bytes.len() <= MAX_PATH_BYTES && !bytes.contains(&0) && path.is_absolute()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_path_is_accepted() {
        // Built from `current_dir` so the test is absolute on every host.
        let absolute = std::env::current_dir().unwrap().join("notes.txt");
        assert_eq!(
            path_arg(Some(absolute.clone().into_os_string())),
            Some(absolute)
        );
    }

    #[test]
    fn a_relative_path_is_rejected() {
        assert_eq!(path_arg(Some(OsString::from("notes.txt"))), None);
        assert_eq!(path_arg(Some(OsString::from("../notes.txt"))), None);
    }

    #[test]
    fn an_empty_or_missing_argument_is_rejected() {
        assert_eq!(path_arg(None), None);
        assert_eq!(path_arg(Some(OsString::new())), None);
    }

    #[test]
    fn an_embedded_nul_is_rejected() {
        let arg = OsString::from("/tmp/a\0b");
        assert_eq!(path_arg(Some(arg)), None);
    }

    #[test]
    fn an_over_long_path_is_rejected() {
        let long = format!("/{}", "a".repeat(MAX_PATH_BYTES));
        assert_eq!(path_arg(Some(OsString::from(long))), None);
    }

    #[test]
    fn file_arg_skips_the_program_flags_and_attempt() {
        let absolute = std::env::current_dir().unwrap().join("notes.txt");
        let args = vec![
            OsString::from("/system/bin/editor"),
            OsString::from("--client"),
            absolute.clone().into_os_string(),
            OsString::from("attempt=1"),
        ];
        assert_eq!(file_arg(args), Some(absolute));
    }

    #[test]
    fn file_arg_is_none_without_a_path() {
        let args = vec![
            OsString::from("/system/bin/files"),
            OsString::from("--client"),
            OsString::from("attempt=1"),
        ];
        assert_eq!(file_arg(args), None);
    }
}
