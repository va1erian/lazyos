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
use super::messenger::Service;

/// The `mimed` service name.
const NAME: &str = "os.lazy.mimed.v1";
/// The structured-error field id `mimed` replies with (outside the generated
/// range).
const ERROR_FIELD: u16 = 15;

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
        let service = Service::connect(NAME)
            .map_err(|code| io::Error::other(format!("mimed unavailable (errno {code})")))?;
        let body = wire::encode_open_args(&wire::OpenArgs {
            path: path.to_owned(),
            verb: "open".to_owned(),
        })
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path too long for mimed"))?;
        let reply = service
            .call(wire::INTERFACE_ID, wire::METHOD_OPEN, ERROR_FIELD, body)
            .map_err(|code| io::Error::other(format!("mimed: errno {code}")))?;
        match wire::decode_open_reply(&reply.body) {
            Ok(reply) if reply.launched && !reply.app.is_empty() => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no application handles this file",
            )),
        }
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
