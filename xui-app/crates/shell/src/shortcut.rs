//! Shortcut files (`<label>.lnk`): what the desktop folder holds instead of
//! hard-coded launchers.
//!
//! The VFS has no symbolic links yet (`kernel/src/fs/vfs.rs`), so a shortcut
//! is a small text file in an INI-like format, one target per file:
//!
//! ```text
//! [Shortcut]
//! App=os.lazy.files
//! ```
//!
//! `App=` names an `init` registry app (launched like its start-menu row);
//! `Path=` names an absolute file or folder (opened like a double-click in
//! Files). The label is the file name without `.lnk`. Shortcut files come
//! from the user's own folder, but anyone who can write there can write
//! one, so parsing is strict: bounded size, one target, a well-formed app id
//! or a clean absolute path, nothing else. A file that does not parse is
//! simply shown as the file it is.

/// The extension that marks a shortcut.
pub const EXTENSION: &str = "lnk";
/// The longest shortcut file read; anything longer is not a shortcut.
pub const MAX_BYTES: u64 = 4096;
/// The longest target path accepted (the kernel's path bound).
const MAX_PATH: usize = 4096;
/// The longest label kept in a file name (bytes, before `.lnk`).
const MAX_LABEL: usize = 120;
/// The section header written (and accepted, optionally) on the first line.
const HEADER: &str = "[Shortcut]";

/// What a shortcut opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// An `init` registry app id (`os.lazy.files`, `terminal`, ...).
    App(String),
    /// An absolute path to a file or folder.
    Path(String),
}

/// Parse a shortcut file's text; `None` unless it names exactly one valid
/// target. Blank lines and `#`/`;` comments are ignored, keys are
/// case-insensitive, and unknown keys are ignored so a newer writer's extras
/// do not break an older shell.
pub fn parse(text: &str) -> Option<Target> {
    if text.len() as u64 > MAX_BYTES {
        return None;
    }
    let mut target = None;
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.eq_ignore_ascii_case(HEADER) && index == 0 {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let (key, value) = (key.trim(), value.trim());
        let next = if key.eq_ignore_ascii_case("app") {
            Target::App(valid_app(value)?.to_owned())
        } else if key.eq_ignore_ascii_case("path") {
            Target::Path(valid_path(value)?.to_owned())
        } else {
            continue;
        };
        if target.replace(next).is_some() {
            return None;
        }
    }
    target
}

/// The file text for `target`.
pub fn encode(target: &Target) -> String {
    match target {
        Target::App(app) => format!("{HEADER}\nApp={app}\n"),
        Target::Path(path) => format!("{HEADER}\nPath={path}\n"),
    }
}

/// An app id a shortcut may name: a short id or a package's system name.
fn valid_app(value: &str) -> Option<&str> {
    (deskmenu::valid_app_id(value) || deskmenu::valid_system_name(value)).then_some(value)
}

/// A path a shortcut may name: absolute, bounded, no control characters.
fn valid_path(value: &str) -> Option<&str> {
    let ok = value.starts_with('/')
        && value.len() <= MAX_PATH
        && !value.chars().any(char::is_control);
    ok.then_some(value)
}

/// Whether `name` is a shortcut's file name (`*.lnk`, case-insensitive).
pub fn is_shortcut_name(name: &str) -> bool {
    label_of(name).is_some()
}

/// The label a shortcut file name shows (`Files.lnk` -> `Files`).
pub fn label_of(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    (ext.eq_ignore_ascii_case(EXTENSION) && !stem.is_empty()).then_some(stem)
}

/// The file name a shortcut labelled `label` is saved under: the label with
/// path separators and control characters dropped, no leading dot (which
/// would hide it), bounded, plus `.lnk`. `None` when nothing usable is left.
pub fn file_name(label: &str) -> Option<String> {
    let mut clean: String = label
        .chars()
        .filter(|c| *c != '/' && *c != '\\' && !c.is_control())
        .collect();
    clean = clean.trim().trim_start_matches('.').trim().to_owned();
    while clean.len() > MAX_LABEL {
        clean.pop();
    }
    (!clean.is_empty()).then(|| format!("{clean}.{EXTENSION}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_and_path_shortcuts_round_trip() {
        for target in [
            Target::App("os.lazy.files".into()),
            Target::App("terminal".into()),
            Target::Path("/home/user/notes.txt".into()),
        ] {
            assert_eq!(parse(&encode(&target)), Some(target));
        }
    }

    #[test]
    fn the_header_comments_and_case_are_optional() {
        assert_eq!(
            parse("# mine\napp = org.lazy.doom\n"),
            Some(Target::App("org.lazy.doom".into()))
        );
        assert_eq!(
            parse("[shortcut]\r\n; x\r\nPATH=/tmp\r\nIcon=whatever\r\n"),
            Some(Target::Path("/tmp".into()))
        );
    }

    #[test]
    fn anything_else_is_not_a_shortcut() {
        for text in [
            "",
            "[Shortcut]\n",
            "App=Bad Id\n",
            "App=../../bin/sh\n",
            "Path=relative/x\n",
            "Path=/a\u{1b}[2J\n",
            "App=terminal\nApp=os.lazy.files\n",
            "App=terminal\nPath=/tmp\n",
            "just some text\n",
            "x\n[Shortcut]\nApp=terminal\n",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
        let huge = format!("App=terminal\n#{}\n", "x".repeat(MAX_BYTES as usize));
        assert_eq!(parse(&huge), None, "over the size bound");
    }

    #[test]
    fn file_names_are_safe_and_labels_come_back() {
        assert_eq!(file_name("Files").as_deref(), Some("Files.lnk"));
        assert_eq!(
            file_name("System Monitor").as_deref(),
            Some("System Monitor.lnk")
        );
        assert_eq!(file_name("../etc/x").as_deref(), Some("etcx.lnk"));
        assert_eq!(file_name(" .hidden ").as_deref(), Some("hidden.lnk"));
        assert_eq!(file_name("a\nb").as_deref(), Some("ab.lnk"));
        assert_eq!(file_name("/./"), None);
        assert!(file_name(&"x".repeat(500)).unwrap().len() <= MAX_LABEL + 4);
        assert_eq!(label_of("Files.lnk"), Some("Files"));
        assert_eq!(label_of("Paint.LNK"), Some("Paint"));
        assert_eq!(label_of(".lnk"), None);
        assert_eq!(label_of("notes.txt"), None);
        assert!(is_shortcut_name("A b.lnk") && !is_shortcut_name("lnk"));
    }
}
