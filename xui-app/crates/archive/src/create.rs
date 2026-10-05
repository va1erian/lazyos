//! Creating a new archive from files and folders on disk.
//!
//! The archive is written to a temporary sibling (`.<name>.partial-<pid>`)
//! and renamed into place only when every member was written, so a failed or
//! cancelled create never leaves a half archive under the chosen name.

use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::extract::Report;
use crate::format::{Format, Level};
use crate::progress::{Counted, Progress};
use crate::source::{plan, total_size, PlanItem, PlanKind, Source};
use crate::writer::{self, EntryWriter};

/// The temporary sibling an archive at `dest` is written to.
pub fn partial_path(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_owned());
    dest.with_file_name(format!(".{name}.partial-{}", std::process::id()))
}

/// Write `sources` as a new `format` archive at `dest`, replacing any file
/// there once the new archive is complete.
pub fn create(
    dest: &Path,
    format: Format,
    level: Level,
    sources: &[Source],
    progress: &Arc<Progress>,
) -> Result<Report> {
    let (items, skipped) = plan(sources, Some(dest));
    progress.set_total(total_size(&items));
    let partial = partial_path(dest);
    let outcome = if format.single_file() {
        write_single(&partial, format, level, &items, progress)
    } else {
        writer::open(format, level, &partial)
            .and_then(|writer| write_items(writer, &items, progress))
    };
    finish(&partial, dest, outcome)?;
    let mut report = report_for(&items);
    report.skipped = skipped;
    Ok(report)
}

/// Rename `partial` over `dest` when `outcome` succeeded; remove it otherwise.
pub(crate) fn finish(partial: &Path, dest: &Path, outcome: Result<()>) -> Result<()> {
    match outcome {
        Ok(()) => fs::rename(partial, dest).map_err(|error| {
            let _ = fs::remove_file(partial);
            Error::Io(error)
        }),
        Err(error) => {
            let _ = fs::remove_file(partial);
            Err(error)
        }
    }
}

/// Write every planned item into `writer` and finish it.
pub(crate) fn write_items(
    mut writer: Box<dyn EntryWriter>,
    items: &[PlanItem],
    progress: &Arc<Progress>,
) -> Result<()> {
    add_items(writer.as_mut(), items, progress)?;
    writer.finish()
}

/// Write every planned item into `writer`.
pub(crate) fn add_items(
    writer: &mut dyn EntryWriter,
    items: &[PlanItem],
    progress: &Arc<Progress>,
) -> Result<()> {
    for item in items {
        progress.check()?;
        progress.begin(&item.name);
        match &item.kind {
            PlanKind::Dir => writer.dir(&item.name, item.meta)?,
            PlanKind::Symlink { target } => writer.symlink(&item.name, item.meta, target)?,
            PlanKind::File { size } => {
                let file = File::open(&item.path)?;
                let mut data = Counted::new(BufReader::new(file), progress);
                writer.file(&item.name, item.meta, *size, &mut data)?;
            }
        }
    }
    Ok(())
}

/// A single-file archive holds exactly one regular file.
fn write_single(
    partial: &Path,
    format: Format,
    level: Level,
    items: &[PlanItem],
    progress: &Arc<Progress>,
) -> Result<()> {
    let [item] = items else {
        return Err(Error::unsupported(format!(
            "{} holds exactly one file; choose ZIP or a tarball for several",
            format.name()
        )));
    };
    let PlanKind::File { size } = item.kind else {
        return Err(Error::unsupported(format!(
            "{} holds a file, not a folder or a link",
            format.name()
        )));
    };
    progress.begin(&item.name);
    let name = item.name.rsplit('/').next().unwrap_or(&item.name);
    let mut data = Counted::new(BufReader::new(File::open(&item.path)?), progress);
    crate::single::write(partial, format, level, name, item.meta, size, &mut data)
}

/// Counts for a create's report.
pub(crate) fn report_for(items: &[PlanItem]) -> Report {
    let mut report = Report::default();
    for item in items {
        match item.kind {
            PlanKind::Dir => report.dirs += 1,
            PlanKind::File { size } => {
                report.files += 1;
                report.bytes += size;
            }
            PlanKind::Symlink { .. } => report.links += 1,
        }
    }
    report
}
