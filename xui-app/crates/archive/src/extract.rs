//! Extracting a selection to a folder, and testing an archive.
//!
//! Extraction follows [`crate::safety`]: unsafe names, encrypted members and
//! special files are skipped with a reason, files are created fresh (never
//! opened through an existing link), and symlinks come last, only when they
//! stay inside the destination. A member whose data turns out damaged is
//! removed and reported; the others are still extracted.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::archive::Archive;
use crate::entry::{Entry, EntryKind};
use crate::error::{Error, Result};
use crate::progress::Progress;
use crate::safety::{self, ensure_dir, prepare, symlink_stays_inside};

/// What to do when a file being extracted already exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Overwrite {
    /// Replace it.
    #[default]
    Replace,
    /// Keep it and skip the entry.
    Skip,
    /// Keep it and write `name (2).ext` beside it.
    Rename,
}

/// How to extract.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// An archive folder whose prefix is removed from every entry path, so
    /// extracting `docs/a.txt` while browsing `docs` writes `a.txt`.
    pub strip: String,
    /// What to do about existing files.
    pub overwrite: Overwrite,
}

/// What an extraction or a test did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub files: u64,
    pub dirs: u64,
    pub links: u64,
    pub bytes: u64,
    /// Entries not written (or failing their test), with the reason.
    pub skipped: Vec<(String, String)>,
    /// The top-level paths created in the destination, in name order.
    pub top_level: Vec<PathBuf>,
}

impl Report {
    /// One line for a status bar.
    pub fn summary(&self, verb: &str) -> String {
        let mut text = format!(
            "{verb} {} file{}",
            self.files,
            if self.files == 1 { "" } else { "s" }
        );
        if self.dirs > 0 {
            text.push_str(&format!(
                ", {} folder{}",
                self.dirs,
                if self.dirs == 1 { "" } else { "s" }
            ));
        }
        if !self.skipped.is_empty() {
            text.push_str(&format!(", {} skipped", self.skipped.len()));
        }
        text
    }
}

/// `entry`'s path relative to the extraction root.
fn relative<'a>(entry: &'a Entry, strip: &str) -> &'a str {
    if strip.is_empty() || !entry.is_under(strip) || entry.path == strip {
        return &entry.path;
    }
    &entry.path[strip.len() + 1..]
}

/// Extract the entries `wanted` selects from `archive` into `dest` (created
/// if missing).
pub fn extract(
    archive: &Archive,
    wanted: &dyn Fn(&Entry) -> bool,
    dest: &Path,
    options: &Options,
    progress: &Arc<Progress>,
) -> Result<Report> {
    fs::create_dir_all(dest)?;
    let mut report = Report::default();
    let mut top_level = BTreeSet::new();
    let mut written: HashMap<String, PathBuf> = HashMap::new();
    // (entry path, relative path, link target, where it goes).
    let mut links: Vec<(String, String, String, PathBuf)> = Vec::new();
    archive.visit(wanted, progress, &mut |entry, data| {
        // The stripped folder itself is the destination: nothing to write.
        if !options.strip.is_empty() && entry.path == options.strip {
            return Ok(());
        }
        let rel = relative(entry, &options.strip).to_owned();
        let refuse = |reason: &str, report: &mut Report| {
            report.skipped.push((entry.path.clone(), reason.to_owned()));
            Ok(())
        };
        if entry.unsafe_path {
            return refuse("its name leaves the folder", &mut report);
        }
        if entry.encrypted {
            return refuse("encrypted", &mut report);
        }
        let target = match prepare(dest, &rel) {
            Ok(target) => target,
            Err(reason) => return refuse(&reason, &mut report),
        };
        match &entry.kind {
            EntryKind::Dir => match ensure_dir(&target) {
                Ok(()) => report.dirs += 1,
                Err(reason) => return refuse(&reason, &mut report),
            },
            EntryKind::File => match write_file(&target, data, options.overwrite, entry)? {
                Written::Done(path, bytes) => {
                    report.files += 1;
                    report.bytes += bytes;
                    written.insert(entry.path.clone(), path);
                }
                Written::Skipped(reason) => return refuse(&reason, &mut report),
            },
            EntryKind::Symlink { target: link } => {
                if !symlink_stays_inside(&rel, link) {
                    return refuse("its link points outside the folder", &mut report);
                }
                links.push((
                    entry.path.clone(),
                    rel.clone(),
                    link.clone(),
                    target.clone(),
                ));
            }
            EntryKind::Hardlink { target: original } => match written.get(original) {
                Some(source) => {
                    let source = source.clone();
                    match copy_hardlink(&source, &target, options.overwrite) {
                        Ok(path) => {
                            report.files += 1;
                            written.insert(entry.path.clone(), path);
                        }
                        Err(reason) => return refuse(&reason, &mut report),
                    }
                }
                None => return refuse("its link target was not extracted", &mut report),
            },
            EntryKind::Special => return refuse("a device or special file", &mut report),
        }
        if let Some(first) = rel.split('/').next() {
            top_level.insert(dest.join(first));
        }
        Ok(())
    })?;
    // A link whose target passes through another link (this archive's, or
    // one already on disk) could resolve outside the destination even though
    // its text stays inside: `a/b -> .` then `c -> a/b/../..`.
    let link_paths: HashSet<String> = links.iter().map(|(_, rel, _, _)| rel.clone()).collect();
    for (name, rel, link, path) in links {
        let is_link = |prefix: &str| link_paths.contains(prefix);
        if safety::walks_through_link(dest, &rel, &link, &is_link) {
            report
                .skipped
                .push((name, "its link points through another link".to_owned()));
            continue;
        }
        match make_symlink(&link, &path, options.overwrite) {
            Ok(()) => report.links += 1,
            Err(reason) => report.skipped.push((name, reason)),
        }
    }
    report.top_level = top_level.into_iter().collect();
    Ok(report)
}

enum Written {
    Done(PathBuf, u64),
    Skipped(String),
}

/// Write one file's data to `target` under the overwrite policy. Damaged
/// data removes the partial file and is reported as a skip; cancelling and
/// failing writes end the extraction.
fn write_file(
    target: &Path,
    data: &mut dyn Read,
    overwrite: Overwrite,
    entry: &Entry,
) -> Result<Written> {
    let target = match fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_dir() => {
            return Ok(Written::Skipped("a folder is in the way".to_owned()))
        }
        Ok(_) => match overwrite {
            Overwrite::Skip => return Ok(Written::Skipped("it already exists".to_owned())),
            Overwrite::Rename => safety::unique(target),
            Overwrite::Replace => target.to_path_buf(),
        },
        Err(_) => target.to_path_buf(),
    };
    // The data goes to a fresh sibling first and replaces the target only
    // once it is complete, so a damaged member never costs the file it
    // would have replaced. The rename replaces a link itself, never writing
    // through it.
    let partial = partial_sibling(&target);
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)?;
    let copied = io::copy(data, &mut out);
    drop(out);
    match copied {
        Ok(bytes) => {
            if let Err(error) = fs::rename(&partial, &target) {
                let _ = fs::remove_file(&partial);
                return Err(Error::Io(error));
            }
            finish_file(&target, entry);
            Ok(Written::Done(target, bytes))
        }
        Err(error) => {
            let _ = fs::remove_file(&partial);
            match Error::from(error) {
                Error::Corrupt(reason) | Error::Unsupported(reason) => Ok(Written::Skipped(reason)),
                other => Err(other),
            }
        }
    }
}

/// A unique temporary name beside `target` for a file being written.
fn partial_sibling(target: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    target.with_file_name(format!(".{name}.part-{}-{n}", std::process::id()))
}

/// Restore what the archive says about a file's mode and time, best-effort
/// (a filesystem without them keeps its defaults).
fn finish_file(path: &Path, entry: &Entry) {
    // The time first: setting it reopens the file for writing, which a
    // read-only mode would refuse.
    if let Some(modified) = entry.modified.and_then(|t| u64::try_from(t).ok()) {
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(modified);
        if let Ok(file) = OpenOptions::new().write(true).open(path) {
            let _ = file.set_modified(time);
        }
    }
    #[cfg(unix)]
    if let Some(mode) = entry.mode {
        use std::os::unix::fs::PermissionsExt;
        // Never restore set-id or sticky bits from an archive.
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777));
    }
}

fn copy_hardlink(
    source: &Path,
    target: &Path,
    overwrite: Overwrite,
) -> std::result::Result<PathBuf, String> {
    let target = match fs::symlink_metadata(target) {
        Ok(_) if overwrite == Overwrite::Skip => return Err("it already exists".to_owned()),
        Ok(_) if overwrite == Overwrite::Rename => safety::unique(target),
        Ok(meta) if meta.file_type().is_dir() => return Err("a folder is in the way".to_owned()),
        Ok(_) => {
            fs::remove_file(target).map_err(|e| e.to_string())?;
            target.to_path_buf()
        }
        Err(_) => target.to_path_buf(),
    };
    fs::copy(source, &target).map_err(|e| e.to_string())?;
    Ok(target)
}

#[cfg(unix)]
fn make_symlink(link: &str, path: &Path, overwrite: Overwrite) -> std::result::Result<(), String> {
    if fs::symlink_metadata(path).is_ok() {
        match overwrite {
            Overwrite::Replace if !path.is_dir() || path.is_symlink() => {
                fs::remove_file(path).map_err(|e| e.to_string())?
            }
            _ => return Err("it already exists".to_owned()),
        }
    }
    std::os::unix::fs::symlink(link, path).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn make_symlink(
    _link: &str,
    _path: &Path,
    _overwrite: Overwrite,
) -> std::result::Result<(), String> {
    Err("symlinks are not supported here".to_owned())
}

/// Read every entry to the end, checking every checksum, writing nothing.
pub fn test(archive: &Archive, progress: &Arc<Progress>) -> Result<Report> {
    let mut report = Report::default();
    archive.visit(&|_| true, progress, &mut |entry, data| {
        if entry.encrypted {
            report
                .skipped
                .push((entry.path.clone(), "encrypted".to_owned()));
            return Ok(());
        }
        match io::copy(data, &mut io::sink()) {
            Ok(bytes) => {
                if matches!(entry.kind, EntryKind::File) {
                    report.files += 1;
                    report.bytes += bytes;
                } else if entry.kind.is_dir() {
                    report.dirs += 1;
                }
                Ok(())
            }
            Err(error) => match Error::from(error) {
                Error::Corrupt(reason) | Error::Unsupported(reason) => {
                    report.skipped.push((entry.path.clone(), reason));
                    Ok(())
                }
                other => Err(other),
            },
        }
    })?;
    Ok(report)
}
