//! The filesystem half of `Install` and `Remove`: extracting a package under
//! `/apps`, publishing its documentation under `/docs/apps`, and deleting both
//! again, generic over [`TreeFs`].
//!
//! `pkgd` binds [`TreeFs`] to its syscalls (`user/src/bin/pkgd/store.rs`); the
//! host tests bind it to an in-memory tree and to `libs/ext2fs`, and the
//! kernel suite to the VFS over ext2 (`ext2_suite::pkg_tree`), so the soak of
//! install/upgrade/remove cycles runs the code `pkgd` runs. Every path is
//! composed through [`crate::layout`] and [`crate::docs`], and every removal
//! is confined to strictly below `/apps` or `/docs/apps` ([`deletable`]); the
//! ext2 volume has no symlinks, so a lexically safe path is a physically safe
//! one.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::access::{under, well_formed};
use crate::docs::{self, Repair};
use crate::layout::{self, APPS_ROOT, DATA_MODE, DOCS_ROOT};

/// Deepest directory tree [`remove_tree`] walks (a package holds at most 255
/// bytes of name, so this is generous).
const MAX_DEPTH: usize = 24;
/// Listings per directory before [`remove_tree`] gives up (a listing may be
/// bounded, so a huge directory needs several rounds).
const MAX_ROUNDS: usize = 64;

/// What a path is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    /// A regular file of this many bytes.
    File(u64),
    Dir,
}

/// The filesystem calls the tree operations need, as the caller's identity
/// (root, for `pkgd`) performs them.
pub trait TreeFs {
    type Error: core::fmt::Debug;
    /// What `path` is, `None` when it does not exist.
    fn stat(&mut self, path: &str) -> Result<Option<Node>, Self::Error>;
    /// Create the directory `path` (its parent exists).
    fn mkdir(&mut self, path: &str) -> Result<(), Self::Error>;
    /// Create or replace the file `path` with `data`.
    fn write(&mut self, path: &str, data: &[u8]) -> Result<(), Self::Error>;
    /// Append `data` to the end of the existing file `path`.
    fn append(&mut self, path: &str, data: &[u8]) -> Result<(), Self::Error>;
    /// Set the permission bits of `path`.
    fn chmod(&mut self, path: &str, mode: u16) -> Result<(), Self::Error>;
    /// The entry names in the directory `path`, without `.` and `..`.
    fn list(&mut self, path: &str) -> Result<Vec<String>, Self::Error>;
    /// Delete the file or empty directory `path`.
    fn remove(&mut self, path: &str) -> Result<(), Self::Error>;
    /// Rename `from` to `to` (which does not exist).
    fn rename(&mut self, from: &str, to: &str) -> Result<(), Self::Error>;
}

/// What is extracted: a validated package, or a test's stand-in.
pub trait Source {
    /// Every entry, `(name, is_dir)`, in archive order.
    fn entries(&self) -> Vec<(&str, bool)>;
    /// The bytes of the file entry `name`.
    fn read(&self, name: &str) -> Result<Vec<u8>, String>;
    /// [`Source::read`] handed to `sink` in order, in pieces. A source that
    /// can unpack piece by piece overrides it, so extraction never holds a
    /// whole file (see [`extract`]). `Err(None)` means `sink` refused a piece.
    fn read_chunks(
        &self,
        name: &str,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), Refused>,
    ) -> Result<(), Option<String>> {
        let data = self.read(name).map_err(Some)?;
        sink(&data).map_err(|Refused| None)
    }
}

/// What a [`Source::read_chunks`] sink returns to stop the stream; the sink
/// keeps the reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused;

impl Source for lazypkg::Package<'_> {
    fn entries(&self) -> Vec<(&str, bool)> {
        lazypkg::Package::entries(self)
            .map(|entry| (entry.name, entry.is_dir))
            .collect()
    }

    fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        lazypkg::Package::read(self, name).map_err(|error| format!("{error}"))
    }

    fn read_chunks(
        &self,
        name: &str,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), Refused>,
    ) -> Result<(), Option<String>> {
        lazypkg::Package::read_chunks(self, name, sink).map_err(|error| match error {
            lazypkg::ChunkError::Read(error) => Some(format!("{error}")),
            lazypkg::ChunkError::Sink(Refused) => None,
        })
    }
}

/// Why a tree operation stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum TreeError<E> {
    /// A filesystem call failed while doing `step`.
    Fs { step: String, error: E },
    /// Something other than a filesystem error, already worded.
    Bad(String),
}

impl<E> TreeError<E> {
    /// The step, for a failure text (`pkgd` appends the errno's words).
    pub fn step(&self) -> &str {
        match self {
            TreeError::Fs { step, .. } | TreeError::Bad(step) => step,
        }
    }
}

fn fs_err<E>(step: impl Into<String>) -> impl FnOnce(E) -> TreeError<E> {
    let step = step.into();
    move |error| TreeError::Fs { step, error }
}

/// Whether the tree operations may delete `path`: an install or app directory
/// strictly below `/apps`, or an app's documentation directory strictly below
/// `/docs/apps` (`docs::deletable`). Nothing else, ever.
pub fn deletable(path: &str) -> bool {
    (well_formed(path) && under(path, APPS_ROOT)) || docs::deletable(path)
}

/// Create `path` unless it already is a directory.
pub fn ensure_dir<F: TreeFs>(fs: &mut F, path: &str) -> Result<(), TreeError<F::Error>> {
    match fs.stat(path).map_err(fs_err(format!("checking {path}")))? {
        Some(Node::Dir) => Ok(()),
        Some(Node::File(_)) => Err(TreeError::Bad(format!("{path} is a file, not a folder"))),
        None => fs.mkdir(path).map_err(fs_err(format!("creating {path}"))),
    }
}

/// Unpack the file entry `name` of `source` to `path` as it is inflated (the
/// first piece creates or replaces the file, the others are appended), check
/// its size on disk and give it `mode`. So a long-lived caller (`pkgd`, whose
/// heap never reuses a block over 1 MiB) holds one piece at a time, never a
/// whole file. A failure part-way leaves a partial file, which the caller
/// removes with the rest of the tree.
fn unpack<F: TreeFs, S: Source + ?Sized>(
    fs: &mut F,
    source: &S,
    name: &str,
    path: &str,
    mode: u16,
) -> Result<(), TreeError<F::Error>> {
    let mut written = 0u64;
    let mut failed = None;
    let streamed = source.read_chunks(name, &mut |piece| {
        let result = if written == 0 {
            fs.write(path, piece)
        } else {
            fs.append(path, piece)
        };
        written += piece.len() as u64;
        result.map_err(|error| {
            failed = Some(error);
            Refused
        })
    });
    match streamed {
        Ok(()) => {}
        Err(Some(error)) => return Err(TreeError::Bad(format!("unpacking {name}: {error}"))),
        Err(None) => {
            let step = format!("writing {name}");
            return Err(match failed {
                Some(error) => TreeError::Fs { step, error },
                None => TreeError::Bad(step),
            });
        }
    }
    if written == 0 {
        // An empty entry has no piece: create the file still.
        fs.write(path, &[])
            .map_err(fs_err(format!("writing {name}")))?;
    }
    match fs.stat(path).map_err(fs_err(format!("checking {name}")))? {
        Some(Node::File(size)) if size == written => {}
        _ => {
            return Err(TreeError::Bad(format!(
                "writing {name}: the file on disk has the wrong size"
            )))
        }
    }
    fs.chmod(path, mode)
        .map_err(fs_err(format!("setting the mode of {name}")))
}

/// Extract every entry of `source` under `install_path` (an
/// `layout::install_path`): directories first (parents before children), then
/// each file, unpacked piece by piece (so memory is one piece, whatever the
/// file's size), verified by size after the write and given its mode
/// (`layout::file_mode`: 0755 under `bin/`, 0644 elsewhere, set before the app
/// is activated, so it is never registered with a program `init` cannot
/// start). Returns the number of files written. The caller removes
/// `install_path` when this fails part-way.
pub fn extract<F: TreeFs, S: Source + ?Sized>(
    fs: &mut F,
    source: &S,
    install_path: &str,
) -> Result<usize, TreeError<F::Error>> {
    let app_dir = install_path
        .rsplit_once('/')
        .map_or(install_path, |(parent, _)| parent);
    ensure_dir(fs, APPS_ROOT)?;
    ensure_dir(fs, app_dir)?;
    ensure_dir(fs, install_path)?;
    let entries = source.entries();
    let dirs = layout::directories(entries.iter().copied())
        .map_err(|error| TreeError::Bad(format!("planning the extraction: {error}")))?;
    for dir in &dirs {
        let path = layout::entry_path(install_path, dir)
            .map_err(|error| TreeError::Bad(format!("creating {dir}: {error}")))?;
        ensure_dir(fs, &path)?;
    }
    let mut written = 0;
    for &(name, _) in entries.iter().filter(|(_, is_dir)| !is_dir) {
        let path = layout::entry_path(install_path, name)
            .map_err(|error| TreeError::Bad(format!("writing {name}: {error}")))?;
        unpack(fs, source, name, &path, layout::file_mode(name))?;
        written += 1;
    }
    Ok(written)
}

/// Delete `path` and everything under it: files first, then directories,
/// deepest first. A path that is already gone is success. Refuses anything
/// [`deletable`] refuses.
pub fn remove_tree<F: TreeFs>(fs: &mut F, path: &str) -> Result<(), TreeError<F::Error>> {
    if !deletable(path) {
        return Err(TreeError::Bad(format!(
            "{path} is outside what pkgd may delete"
        )));
    }
    remove_at(fs, path, 0)
}

fn remove_at<F: TreeFs>(fs: &mut F, path: &str, depth: usize) -> Result<(), TreeError<F::Error>> {
    if depth > MAX_DEPTH {
        return Err(TreeError::Bad(format!("{path} is nested too deeply")));
    }
    match fs.stat(path).map_err(fs_err(format!("checking {path}")))? {
        None => return Ok(()),
        Some(Node::File(_)) => return fs.remove(path).map_err(fs_err(format!("deleting {path}"))),
        Some(Node::Dir) => {}
    }
    for _ in 0..MAX_ROUNDS {
        let names = fs.list(path).map_err(fs_err(format!("listing {path}")))?;
        if names.is_empty() {
            break;
        }
        for name in names {
            if name.is_empty() || name == "." || name == ".." || name.contains('/') {
                return Err(TreeError::Bad(format!("{path} lists a strange entry")));
            }
            remove_at(fs, &format!("{path}/{name}"), depth + 1)?;
        }
    }
    fs.remove(path).map_err(fs_err(format!("deleting {path}")))
}

/// Remove `path` when it is an empty directory [`deletable`] allows;
/// anything else is left alone.
pub fn remove_if_empty<F: TreeFs>(fs: &mut F, path: &str) {
    if deletable(path) && fs.list(path).is_ok_and(|names| names.is_empty()) {
        let _ = fs.remove(path);
    }
}

/// Write the package's `docs/**.md` to `/docs/apps/<system_name>~new`
/// (replacing a leftover copy). Returns how many files it holds; with none,
/// nothing is staged. Nothing live changes until [`commit_docs`].
pub fn stage_docs<F: TreeFs, S: Source + ?Sized>(
    fs: &mut F,
    source: &S,
    system_name: &str,
) -> Result<usize, TreeError<F::Error>> {
    let staging =
        docs::staging_dir(system_name).map_err(|error| TreeError::Bad(format!("{error}")))?;
    remove_tree(fs, &staging)?;
    let mut files: Vec<(&str, &str)> = Vec::new();
    for (name, is_dir) in source.entries() {
        let doc = docs::doc_file(name, is_dir)
            .map_err(|error| TreeError::Bad(format!("{name}: {error}")))?;
        if let Some(relative) = doc {
            files.push((name, relative));
        }
    }
    if files.is_empty() {
        return Ok(0);
    }
    ensure_dir(fs, DOCS_ROOT)?;
    ensure_dir(fs, &staging)?;
    let dirs = layout::directories(files.iter().map(|&(_, relative)| (relative, false)))
        .map_err(|error| TreeError::Bad(format!("planning the documentation: {error}")))?;
    for dir in &dirs {
        let path = layout::entry_path(&staging, dir)
            .map_err(|error| TreeError::Bad(format!("{dir}: {error}")))?;
        ensure_dir(fs, &path)?;
    }
    for &(name, relative) in &files {
        let path = layout::entry_path(&staging, relative)
            .map_err(|error| TreeError::Bad(format!("{name}: {error}")))?;
        unpack(fs, source, name, &path, DATA_MODE)?;
    }
    Ok(files.len())
}

/// Make the staged documentation the live one: the live directory moves to
/// `<system_name>~old`, the copy takes its name, then the old tree is
/// deleted. With nothing staged (`staged == 0`, a version without docs), the
/// live directory is deleted.
pub fn commit_docs<F: TreeFs>(
    fs: &mut F,
    system_name: &str,
    staged: usize,
) -> Result<(), TreeError<F::Error>> {
    let bad = |error: layout::PathError| TreeError::Bad(format!("{error}"));
    let live = docs::docs_dir(system_name).map_err(bad)?;
    if staged == 0 {
        return remove_tree(fs, &live);
    }
    let staging = docs::staging_dir(system_name).map_err(bad)?;
    let retired = docs::retired_dir(system_name).map_err(bad)?;
    remove_tree(fs, &retired)?;
    if fs
        .stat(&live)
        .map_err(fs_err(format!("checking {live}")))?
        .is_some()
    {
        fs.rename(&live, &retired)
            .map_err(fs_err(format!("setting {live} aside")))?;
    }
    fs.rename(&staging, &live)
        .map_err(fs_err(format!("publishing {live}")))?;
    remove_tree(fs, &retired)
}

/// Delete an app's documentation: the live directory and any copy.
pub fn withdraw_docs<F: TreeFs>(fs: &mut F, system_name: &str) -> Result<(), TreeError<F::Error>> {
    let bad = |error: layout::PathError| TreeError::Bad(format!("{error}"));
    remove_tree(fs, &docs::staging_dir(system_name).map_err(bad)?)?;
    remove_tree(fs, &docs::retired_dir(system_name).map_err(bad)?)?;
    remove_tree(fs, &docs::docs_dir(system_name).map_err(bad)?)
}

/// Repair what a stop in the middle of [`commit_docs`] or [`stage_docs`] left
/// in `/docs/apps` (`docs::recovery`). Returns the repairs made.
pub fn repair_docs<F: TreeFs>(fs: &mut F) -> Result<usize, TreeError<F::Error>> {
    if fs
        .stat(DOCS_ROOT)
        .map_err(fs_err(format!("checking {DOCS_ROOT}")))?
        != Some(Node::Dir)
    {
        return Ok(0);
    }
    let names = fs
        .list(DOCS_ROOT)
        .map_err(fs_err(format!("listing {DOCS_ROOT}")))?;
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let repairs = docs::recovery(&names);
    for repair in &repairs {
        match repair {
            Repair::Remove(path) => remove_tree(fs, path)?,
            Repair::Rename { from, to } => fs
                .rename(from, to)
                .map_err(fs_err(format!("restoring {to}")))?,
        }
    }
    Ok(repairs.len())
}
