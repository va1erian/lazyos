//! Files and folders on disk to put into an archive, and the walk that turns
//! them into a plan of members.
//!
//! The walk never follows a symlink: a link is archived as a link. A path the
//! walk cannot read is reported and skipped rather than failing the whole
//! operation, as 7-Zip does.

use std::fs;
use std::path::{Path, PathBuf};

use crate::entry::normalize;
use crate::writer::Meta;

/// One file or folder to add, and the archive path it goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Where it is on disk.
    pub path: PathBuf,
    /// Its archive path (normalised; a folder's children go below it).
    pub name: String,
}

impl Source {
    /// Sources for `paths`, each named by its last component and placed
    /// below `folder` (a normalised archive path, `""` for the root).
    pub fn under(folder: &str, paths: &[PathBuf]) -> Vec<Source> {
        paths
            .iter()
            .map(|path| {
                let leaf = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "_".to_owned());
                let joined = if folder.is_empty() {
                    leaf
                } else {
                    format!("{folder}/{leaf}")
                };
                Source {
                    path: path.clone(),
                    name: normalize(&joined).0,
                }
            })
            .collect()
    }
}

/// What a planned member is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanKind {
    Dir,
    File { size: u64 },
    Symlink { target: String },
}

/// One member to write, with the file it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanItem {
    pub path: PathBuf,
    pub name: String,
    pub kind: PlanKind,
    pub meta: Meta,
}

/// The members `sources` expand to (folders recursively, children sorted by
/// name), and the paths that could not be read with the reason. `exclude` is
/// never added (the archive being written).
pub fn plan(sources: &[Source], exclude: Option<&Path>) -> (Vec<PlanItem>, Vec<(String, String)>) {
    let mut items = Vec::new();
    let mut skipped = Vec::new();
    let exclude = exclude.and_then(|path| fs::canonicalize(path).ok());
    for source in sources {
        visit(
            &source.path,
            &source.name,
            exclude.as_deref(),
            &mut items,
            &mut skipped,
        );
    }
    (items, skipped)
}

fn visit(
    path: &Path,
    name: &str,
    exclude: Option<&Path>,
    items: &mut Vec<PlanItem>,
    skipped: &mut Vec<(String, String)>,
) {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) => {
            skipped.push((name.to_owned(), error.to_string()));
            return;
        }
    };
    if exclude.is_some() && fs::canonicalize(path).ok().as_deref() == exclude {
        skipped.push((name.to_owned(), "the archive itself".to_owned()));
        return;
    }
    let item_meta = Meta {
        modified: meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|duration| i64::try_from(duration.as_secs()).ok()),
        mode: mode_of(&meta),
    };
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        match fs::read_link(path) {
            Ok(target) => items.push(PlanItem {
                path: path.to_path_buf(),
                name: name.to_owned(),
                kind: PlanKind::Symlink {
                    target: target.to_string_lossy().into_owned(),
                },
                meta: item_meta,
            }),
            Err(error) => skipped.push((name.to_owned(), error.to_string())),
        }
    } else if file_type.is_dir() {
        items.push(PlanItem {
            path: path.to_path_buf(),
            name: name.to_owned(),
            kind: PlanKind::Dir,
            meta: item_meta,
        });
        let mut children = match fs::read_dir(path) {
            Ok(children) => children.filter_map(|child| child.ok()).collect::<Vec<_>>(),
            Err(error) => {
                skipped.push((name.to_owned(), error.to_string()));
                return;
            }
        };
        children.sort_by_key(|child| child.file_name());
        for child in children {
            let child_name = format!("{name}/{}", child.file_name().to_string_lossy());
            visit(
                &child.path(),
                &normalize(&child_name).0,
                exclude,
                items,
                skipped,
            );
        }
    } else if file_type.is_file() {
        items.push(PlanItem {
            path: path.to_path_buf(),
            name: name.to_owned(),
            kind: PlanKind::File { size: meta.len() },
            meta: item_meta,
        });
    } else {
        skipped.push((name.to_owned(), "not a regular file".to_owned()));
    }
}

#[cfg(unix)]
fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    Some(if meta.is_dir() {
        0o755
    } else if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    })
}

/// The bytes the files of `items` hold.
pub fn total_size(items: &[PlanItem]) -> u64 {
    items
        .iter()
        .map(|item| match item.kind {
            PlanKind::File { size } => size,
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_named_by_their_leaf_under_the_folder() {
        let sources = Source::under(
            "docs",
            &[PathBuf::from("/home/a/report.txt"), PathBuf::from("pics")],
        );
        assert_eq!(sources[0].name, "docs/report.txt");
        assert_eq!(sources[1].name, "docs/pics");
        assert_eq!(Source::under("", &[PathBuf::from("x")])[0].name, "x");
    }

    #[test]
    fn a_missing_path_is_skipped_not_fatal() {
        let (items, skipped) = plan(
            &[Source {
                path: PathBuf::from("/definitely/not/here"),
                name: "x".into(),
            }],
            None,
        );
        assert!(items.is_empty());
        assert_eq!(skipped.len(), 1);
    }
}
