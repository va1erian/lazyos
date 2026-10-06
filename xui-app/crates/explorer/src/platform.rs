#![forbid(unsafe_code)]

//! The OS seam: everything the explorer needs from a filesystem and a shell.
//!
//! Both traits are object-safe and mention nothing but [`Path`], [`OsString`],
//! [`io::Result`] and [`SystemTime`]: no `xui` type, no `std::fs`, no `cfg`.
//! The portable core holds a `Rc<dyn Platform>` and a `Rc<dyn Launcher>`, so a
//! target such as LazyOS ports the explorer by implementing these two traits
//! (plus a [`Backend`](xui_core::backend::Backend)) and nothing else.
//!
//! [`Session`] is the optional third seam: the clipboard and the selection
//! the explorer shares with other apps; [`NoSession`] is its absence.
//!
//! Deletion never follows a symlink: [`Platform::remove`] removes the link
//! itself, and [`Platform::metadata`] reports a symlink as [`Kind::Symlink`]
//! rather than resolving it.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What a path is, without following a symlink.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link (its target is not resolved).
    Symlink,
}

impl Kind {
    /// Whether this is a directory.
    pub const fn is_dir(self) -> bool {
        matches!(self, Kind::Dir)
    }

    /// The name shown for this kind in a Properties dialog.
    pub const fn label(self) -> &'static str {
        match self {
            Kind::File => "File",
            Kind::Dir => "Folder",
            Kind::Symlink => "Symlink",
        }
    }
}

/// One path's metadata, as reported without following symlinks.
///
/// `name` and `parent` are carried here (filled from the path) so a Properties
/// dialog is built by [`describe`](crate::model::describe) from the metadata
/// alone. `entries` is the number of direct children of a directory, or `None`
/// when unknown or not a directory; it is deliberately not a recursive size.
#[derive(Clone, Debug)]
pub struct Meta {
    /// The path's final component (empty when it has none).
    pub name: OsString,
    /// The path's parent, or `None` for a filesystem root.
    pub parent: Option<PathBuf>,
    /// What the path is.
    pub kind: Kind,
    /// The file's size in bytes, when known (not a directory's).
    pub size: Option<u64>,
    /// The last-modification time, when available.
    pub modified: Option<SystemTime>,
    /// The creation time, when the platform records one.
    pub created: Option<SystemTime>,
    /// Whether the path is marked read-only.
    pub readonly: bool,
    /// A directory's direct-entry count, when known.
    pub entries: Option<usize>,
}

impl Meta {
    /// Metadata for `path` with the fields a lister knows, and no times or
    /// read-only flag: the base a platform fills in.
    pub fn bare(path: &Path, kind: Kind) -> Meta {
        Meta {
            name: path.file_name().map(OsString::from).unwrap_or_default(),
            parent: path.parent().map(Path::to_path_buf),
            kind,
            size: None,
            modified: None,
            created: None,
            readonly: false,
            entries: None,
        }
    }
}

/// One directory entry: its raw name and the metadata describing it.
#[derive(Clone, Debug)]
pub struct RawEntry {
    /// The entry's raw (possibly non-UTF-8) name.
    pub name: OsString,
    /// The entry's metadata.
    pub meta: Meta,
}

/// A filesystem the explorer can list, inspect and delete through.
pub trait Platform {
    /// Lists `dir`, one entry per child, without following symlinks. The order
    /// is unspecified: the core sorts the result.
    fn list(&self, dir: &Path) -> io::Result<Vec<RawEntry>>;

    /// Metadata for `path`, without following a symlink: a link is reported as
    /// [`Kind::Symlink`], never as its target.
    fn metadata(&self, path: &Path) -> io::Result<Meta>;

    /// Deletes `path`. A symlink is removed as a link, never through it. A
    /// directory is removed only when `recursive` is set (and then with its
    /// contents); a non-empty directory with `recursive` clear is an error.
    fn remove(&self, path: &Path, recursive: bool) -> io::Result<()>;

    /// The user's home directory, when the platform has one.
    fn home(&self) -> Option<PathBuf>;
}

/// Opens a file with the operating system's default handler.
pub trait Launcher {
    /// Hands `path` to the OS. `Ok(())` means the request was accepted, not that
    /// the handler ran.
    fn open(&self, path: &Path) -> io::Result<()>;

    /// Called just before a folder window opens because the user activated a
    /// tile in window `window` (the backend's raw window id): the shell may
    /// use it to animate the new window from that tile. `tile` is the tile's
    /// approximate edge in device pixels; the tile is the one under the
    /// window's last pointer position, which is where the activation
    /// happened. Purely cosmetic and best-effort, so the default does nothing
    /// and nothing may fail or block here.
    fn hint_open_origin(&self, _window: u64, _tile: i32) {}
}

/// What a paste did: the top-level items copied in, and what failed (with
/// why). A name already taken in the folder is never overwritten: the copy
/// gets `name (2)` instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pasted {
    /// Top-level items copied into the folder.
    pub copied: usize,
    /// What could not be copied, with why.
    pub failed: Vec<(PathBuf, String)>,
}

/// What the explorer shares with the rest of the user's session: the
/// clipboard (copy and paste of files) and the current selection.
///
/// Every method has a default, so a target with no clipboard or no session
/// bus still builds: copy and paste then report `Unsupported` in the status
/// bar and the selection goes nowhere.
pub trait Session {
    /// Put `paths` (absolute, in view order) on the session clipboard.
    fn copy(&self, _paths: &[PathBuf]) -> io::Result<()> {
        Err(unsupported())
    }

    /// Copy the files the clipboard holds into folder `dir` (folders
    /// recursively). `Ok` with nothing copied and nothing failed means the
    /// clipboard held no files.
    fn paste_into(&self, _dir: &Path) -> io::Result<Pasted> {
        Err(unsupported())
    }

    /// The selection of the window showing `dir` is now `paths` (absolute,
    /// empty when nothing is selected). Best-effort: nothing may fail here.
    fn selection_changed(&self, _dir: &Path, _paths: &[PathBuf]) {}
}

/// The [`Session`] of a target without one.
pub struct NoSession;

impl Session for NoSession {}

fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "no clipboard on this platform")
}
