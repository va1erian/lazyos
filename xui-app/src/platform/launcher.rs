//! The Files app's [`Launcher`]: hand a file to `mimed`, which guesses its MIME
//! type, resolves the registered app and asks `init` to launch it.
//!
//! The portable explorer only knows "open this path with the OS"; on LazyOS
//! that is one `mimed.Open` call over Messenger. `mimed` owns the MIME
//! database and the open-with registry, so this launcher stays a thin client.
//! A path no app handles, or a launch `init` refuses, becomes
//! [`io::ErrorKind::Unsupported`], which the explorer shows in its status bar.

use std::io;
use std::path::Path;

use messenger_generated::os_lazy_mimed_v1 as wire;
use xui_explorer::platform::Launcher;

use super::argv::is_acceptable;
use super::messenger::client;

/// Opens files through `mimed` / `init`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LazyLauncher;

impl LazyLauncher {
    /// The launcher.
    pub const fn new() -> LazyLauncher {
        LazyLauncher
    }
}

impl Launcher for LazyLauncher {
    fn open(&self, path: &Path) -> io::Result<()> {
        // The path comes from the explorer (and so ultimately from a launch
        // argument or a directory entry): validate before it reaches a service.
        if !is_acceptable(path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path must be absolute, NUL-free and bounded",
            ));
        }
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "path is not valid UTF-8")
        })?;
        open_with_mimed(path, "no application handles this file")
    }
}

/// Opens the file at `path` (absolute) with the app registered for its type.
pub fn open_path(path: &str) -> io::Result<()> {
    LazyLauncher.open(Path::new(path))
}

/// Opens `url` in the app registered for its scheme (`https:` in the web
/// browser, `mailto:` in Mail): `mimed` types it `x-scheme-handler/<scheme>`
/// and asks `init` to start that app with the URL as its argument.
pub fn open_url(url: &str) -> io::Result<()> {
    if !is_acceptable_url(url) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a URL LazyOS can hand on",
        ));
    }
    open_with_mimed(url, "no application handles this kind of link")
}

/// Whether `url` is a URL `init` takes as a launch argument: a scheme (a
/// letter, then letters, digits, `+`, `-`, `.`) and `:`, no control
/// character, and short enough. Spaces are allowed but should be encoded.
pub fn is_acceptable_url(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && scheme.len() > 1
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        && url.len() <= MAX_URL_BYTES
        && !url.chars().any(char::is_control)
}

/// The longest URL handed on: `init`'s launch argument limit.
const MAX_URL_BYTES: usize = 1024;

/// `mimed.Open(target, "open")`: guess, resolve and launch. `unhandled` is
/// the error text when no app takes it. The call waits what `idl/mimed.midl`
/// states for `Open`: `mimed` resolves the app and launches, and a full queue
/// or a stalled service answers there instead of holding the UI thread.
fn open_with_mimed(target: &str, unhandled: &str) -> io::Result<()> {
    let mimed = client(wire::Client::connect)
        .map_err(|error| io::Error::other(format!("mimed unavailable (errno {})", error.code())))?;
    let reply = mimed
        .open(target, "open")
        .map_err(|error| io::Error::other(format!("mimed: errno {}", error.code())))?;
    if reply.launched && !reply.app.is_empty() {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Unsupported, unhandled))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_relative_path_is_rejected_before_any_service_call() {
        let error = LazyLauncher
            .open(&PathBuf::from("relative.txt"))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn only_urls_init_accepts_are_handed_on() {
        for url in [
            "https://example.com/a b",
            "mailto:me@example.com",
            "x-y.z+1:rest",
        ] {
            assert!(is_acceptable_url(url), "{url}");
        }
        let long = format!("https://{}", "a".repeat(MAX_URL_BYTES));
        for url in [
            "/tmp/a",
            "C:\\x",
            "1ab:c",
            "no scheme",
            "https://a/\n",
            long.as_str(),
        ] {
            assert!(!is_acceptable_url(url), "{url}");
        }
        let error = open_url("relative").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_non_utf8_path_is_rejected() {
        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let raw = OsStr::from_bytes(b"/tmp/\xff.txt");
            let error = LazyLauncher.open(&PathBuf::from(raw)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }
}
