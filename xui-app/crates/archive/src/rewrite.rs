//! Adding to and deleting from an existing archive.
//!
//! Neither format can be edited in place safely, so a rewrite builds a new
//! archive beside the old one and renames it over it at the end. Kept zip
//! members are copied raw (no recompression); kept tar members stream
//! through. An added file replaces a member of the same path.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom};
use std::sync::Arc;

use crate::archive::Archive;
use crate::create::{add_items, finish, partial_path, report_for};
use crate::entry::{Entry, EntryKind};
use crate::error::{Error, Result};
use crate::extract::Report;
use crate::format::Level;
use crate::progress::{Counted, Progress};
use crate::source::{plan, total_size, Source};
use crate::writer::{self, Meta};
use crate::zip::write::ZipWriter;

/// Rewrite `archive` without the entries `delete` selects and with `add`
/// added (replacing same-path entries). The archive must be reopened after.
pub fn rewrite(
    archive: &Archive,
    delete: &dyn Fn(&Entry) -> bool,
    add: &[Source],
    level: Level,
    progress: &Arc<Progress>,
) -> Result<Report> {
    let format = archive.format;
    if !format.writable() || format.single_file() {
        return Err(Error::unsupported(format!(
            "changing a {} archive",
            format.name()
        )));
    }
    let (items, mut skipped) = plan(add, Some(&archive.path));
    let replaced: HashSet<&str> = items.iter().map(|item| item.name.as_str()).collect();
    let keep = |entry: &Entry| !delete(entry) && !replaced.contains(entry.path.as_str());
    let kept_bytes: u64 = archive
        .entries
        .iter()
        .filter(|e| keep(e))
        .map(|e| e.packed.unwrap_or(e.size))
        .sum();
    let partial = partial_path(&archive.path);
    let outcome = match archive.zip() {
        Some(zip) => (|| {
            let mut writer = ZipWriter::new(BufWriter::new(File::create(&partial)?), level);
            progress.set_total(kept_bytes + total_size(&items));
            for entry in archive.entries.iter().filter(|e| keep(e)) {
                progress.check()?;
                progress.begin(&entry.path);
                let mut file = File::open(&archive.path)?;
                let start = zip.data_offset(&mut file, entry.index)?;
                file.seek(SeekFrom::Start(start))?;
                let mut raw = Counted::new(file, progress);
                writer.raw_copy(&zip.members[entry.index], entry.modified, &mut raw)?;
            }
            add_items(&mut writer, &items, progress)?;
            writer.finish_inner().map(|_| ())
        })(),
        None => (|| {
            let mut writer = writer::open(format, level, &partial)?;
            let mut skipped_kept = Vec::new();
            archive.visit(&keep, progress, &mut |entry, data| {
                let meta = Meta {
                    modified: entry.modified,
                    mode: entry.mode,
                };
                match &entry.kind {
                    EntryKind::Dir => writer.dir(&entry.path, meta),
                    EntryKind::File => writer.file(&entry.path, meta, entry.size, data),
                    EntryKind::Symlink { target } => writer.symlink(&entry.path, meta, target),
                    EntryKind::Hardlink { target } => writer.hardlink(&entry.path, meta, target),
                    EntryKind::Special => {
                        skipped_kept
                            .push((entry.path.clone(), "a special file was dropped".to_owned()));
                        Ok(())
                    }
                }
            })?;
            skipped.extend(skipped_kept);
            // The visit set the total to the old archive's size; the added
            // files come on top.
            let snapshot = progress.snapshot();
            progress.set_total(snapshot.total.max(snapshot.done) + total_size(&items));
            add_items(writer.as_mut(), &items, progress)?;
            writer.finish()
        })(),
    };
    finish(&partial, &archive.path, outcome)?;
    let mut report = report_for(&items);
    report.skipped = skipped;
    Ok(report)
}

/// Whether `entry` is one of `paths` or lies below one of them: the deletion
/// predicate for a selection of rows (a folder row takes its contents).
pub fn under_any(entry: &Entry, paths: &[String]) -> bool {
    paths.iter().any(|path| entry.is_under(path))
}

/// Rewrite `archive` without the entries at or below `paths`.
pub fn delete(archive: &Archive, paths: &[String], progress: &Arc<Progress>) -> Result<Report> {
    rewrite(
        archive,
        &|entry| under_any(entry, paths),
        &[],
        Level::Normal,
        progress,
    )
}

/// Rewrite `archive` with `sources` added.
pub fn add(
    archive: &Archive,
    sources: &[Source],
    level: Level,
    progress: &Arc<Progress>,
) -> Result<Report> {
    rewrite(archive, &|_| false, sources, level, progress)
}
