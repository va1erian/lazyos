#![forbid(unsafe_code)]

//! What a portable app crate takes from its platform, in one trait.
//!
//! The four `Host` structs this replaces (archiver, pdfview, videoview,
//! writer) each carried the same things: the file system the pickers browse,
//! the folder they open in, a way to write and to launch, and a way to say
//! what happened. [`AppHost`] is that, once. `xui_app::AppContext` implements
//! it for LazyOS; [`testing::StdHost`] and [`testing::MemHost`] implement it
//! for host tests.
//!
//! # Evidence
//!
//! A crate under `xui-app/crates/` that `xui-app` depends on cannot call
//! `xui_app::trace` (a dependency cycle). It reports through
//! [`Report::report`] instead: the line is the text after the channel prefix
//! (`OPEN:PASS:/a.pdf`, not `PDF:OPEN:PASS:/a.pdf`), and the host turns it
//! into a `trace` line on the app's channel, byte for byte what the app
//! printed before.

pub mod dialogs;
pub mod error;
pub mod recent;
pub mod settings;
pub mod testing;

use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_core::widget::FileSystem;

/// Somewhere to report evidence to: the whole of what `settings` and
/// `confd-editor` need from their host.
pub trait Report {
    /// One evidence line, without its channel prefix; the host adds the
    /// prefix and writes it through `trace`. Never put a secret in one.
    fn report(&self, line: &str);
}

/// The platform an app crate runs on.
pub trait AppHost: Report {
    /// What the pickers browse.
    fn file_system(&self) -> Rc<dyn FileSystem>;

    /// Where the pickers open when the app has no better folder.
    fn start_dir(&self) -> PathBuf;

    /// A private folder for files the app extracts or drags out (the
    /// Archiver's "open inside"). The caller creates it.
    fn temp_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }

    /// Opens `path` with the system's handler for its type.
    fn launch(&self, _path: &Path) -> Result<(), String> {
        Err("no launcher".to_owned())
    }

    /// Folders never offered as a place to write to (the read-only system
    /// tree on LazyOS); the start folder is offered instead.
    fn read_only_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Writes a whole file, replacing what was there. On LazyOS it writes a
    /// temp file and renames it, refusing symlinks.
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        std::fs::write(path, bytes).map_err(|error| error.to_string())
    }

    /// Whether `dir` is a sensible default destination: not below a
    /// read-only root and not marked read-only.
    fn suggests(&self, dir: &Path) -> bool {
        !self
            .read_only_roots()
            .iter()
            .any(|root| dir.starts_with(root))
            && std::fs::metadata(dir).is_ok_and(|meta| !meta.permissions().readonly())
    }
}

/// A host the app shares between its widgets.
pub type SharedHost = Rc<dyn AppHost>;

/// `host.report(&format!(..))` without the `format!` at the call site.
#[macro_export]
macro_rules! report {
    ($host:expr, $($line:tt)+) => {
        {
            use $crate::Report as _;
            $host.report(&format!($($line)+))
        }
    };
}
