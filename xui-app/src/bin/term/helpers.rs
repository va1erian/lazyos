//! Small pure rules the Terminal's conveniences share, host-tested: which URLs
//! may be handed on, where a new window starts, what a saved file is called.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use xui_app::platform::argv::is_acceptable;

/// The longest URL passed on or shown.
pub const MAX_URL: usize = 2048;

/// The schemes the Terminal will hand to `mimed`. A program can put any
/// scheme in an OSC 8 link; only these open.
pub const SCHEMES: [&str; 4] = ["http", "https", "mailto", "file"];

/// `url` as it may be shown and passed on, or `None`: no control characters,
/// no space or other character that would end a launch argument early, at most
/// [`MAX_URL`], and one of the [`SCHEMES`].
pub fn sanitize_url(url: &str) -> Option<String> {
    if url.is_empty() || url.len() > MAX_URL || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let (scheme, rest) = url.split_once(':')?;
    let scheme = scheme.to_ascii_lowercase();
    (SCHEMES.contains(&scheme.as_str()) && !rest.is_empty()).then(|| url.to_string())
}

/// The directory a Terminal starts in: `--cwd <path>` or a lone absolute path
/// argument (what `init`'s `Launch` passes), if it is an existing directory;
/// else `home` if that is one; else none (the shell's own default).
pub fn start_dir(args: impl IntoIterator<Item = OsString>, home: Option<&Path>, is_dir: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let args: Vec<OsString> = args.into_iter().collect();
    let flagged = args
        .windows(2)
        .find(|w| w[0] == "--cwd")
        .map(|w| PathBuf::from(&w[1]));
    let asked = flagged.or_else(|| xui_app::platform::argv::file_arg(args.clone()));
    let valid = |p: &Path| is_acceptable(p) && is_dir(p);
    asked
        .filter(|p| valid(p))
        .or_else(|| home.filter(|h| valid(h)).map(Path::to_path_buf))
}

/// `YYYY-MM-DD` of a time in seconds since the Unix epoch (UTC), by the
/// days-from-civil algorithm.
pub fn date_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// The name the Save dialog suggests.
pub fn suggested_name(secs: u64) -> String {
    format!("terminal-{}.txt", date_stamp(secs))
}

/// Whether a path from the Save dialog may be written: absolute, bounded, no
/// NUL. (The path only ever comes from the dialog.)
pub fn savable(path: &Path) -> bool {
    is_acceptable(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_pass_only_when_clean_and_of_a_known_scheme() {
        for ok in ["http://example.com/", "https://a.b/c?d=e#f", "mailto:me@example.org", "file:///tmp/x", "HTTP://UP.example/"] {
            assert_eq!(sanitize_url(ok).as_deref(), Some(ok), "{ok}");
        }
        for bad in [
            "",
            "javascript:alert(1)",
            "ssh://host",
            "data:text/html,x",
            "http",
            "http:",
            "http://a b",
            "http://a\nb",
            "http://a\u{1b}[31m",
            "http://a\u{7f}",
            "http://x/\u{85}",
            "\u{0}http://x",
        ] {
            assert_eq!(sanitize_url(bad), None, "{bad:?}");
        }
        let long = format!("http://{}", "a".repeat(MAX_URL));
        assert_eq!(sanitize_url(&long), None);
        assert!(sanitize_url(&format!("http://{}", "a".repeat(MAX_URL - 7))).is_some());
    }

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_new_window_starts_in_an_existing_absolute_directory() {
        let is_dir = |p: &Path| p == Path::new("/tmp") || p == Path::new("/home/user");
        let home = Some(Path::new("/home/user"));
        assert_eq!(start_dir(args(&["xui-term", "/tmp"]), home, is_dir), Some(PathBuf::from("/tmp")));
        assert_eq!(start_dir(args(&["xui-term", "--cwd", "/tmp"]), home, is_dir), Some(PathBuf::from("/tmp")));
        assert_eq!(start_dir(args(&["xui-term", "--client", "/tmp"]), home, is_dir), Some(PathBuf::from("/tmp")));
        // Not a directory, relative, or missing: the home directory.
        assert_eq!(start_dir(args(&["xui-term", "/nonexistent"]), home, is_dir), Some(PathBuf::from("/home/user")));
        assert_eq!(start_dir(args(&["xui-term", "--cwd", "relative/dir"]), home, is_dir), Some(PathBuf::from("/home/user")));
        assert_eq!(start_dir(args(&["xui-term"]), home, is_dir), Some(PathBuf::from("/home/user")));
        // Neither: the shell's default.
        assert_eq!(start_dir(args(&["xui-term", "/nonexistent"]), Some(Path::new("/nope")), is_dir), None);
        assert_eq!(start_dir(args(&["xui-term"]), None, is_dir), None);
        assert_eq!(start_dir(args(&["xui-term", "--cwd"]), None, is_dir), None);
    }

    #[test]
    fn dates_come_out_right() {
        assert_eq!(date_stamp(0), "1970-01-01");
        assert_eq!(date_stamp(951_782_400), "2000-02-29", "a leap day");
        assert_eq!(date_stamp(1_760_227_200), "2025-10-12");
        assert_eq!(date_stamp(4_102_444_800), "2100-01-01");
        assert_eq!(date_stamp(1_709_164_800), "2024-02-29");
        assert_eq!(suggested_name(86_400), "terminal-1970-01-02.txt");
    }

    #[test]
    fn only_absolute_paths_are_written() {
        assert!(savable(Path::new("/home/user/terminal-2025-10-12.txt")));
        assert!(!savable(Path::new("relative.txt")));
        assert!(!savable(Path::new("")));
        assert!(!savable(Path::new("/tmp/a\0b")));
    }
}
