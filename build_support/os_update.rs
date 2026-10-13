//! What an in-place update of the OS volume needs beyond writing the files:
//! the manifest that survives an interrupted update, the check that skips a
//! file that is already there, and the reclaim of what an interrupted update
//! left behind.
//!
//! An update writes the manifest last, because the manifest names what the
//! volume holds. A build that dies halfway (the volume is full, QEMU holds the
//! image, the host loses power) therefore left files no manifest lists, and no
//! later build could free them: every failed update made the next one more
//! likely to fail too. [`with_new`] closes that hole (the manifest names the new
//! paths *before* they are written) and [`reclaim_leftovers`] repairs a volume
//! an older build already left that way.

use std::collections::BTreeSet;

use ext2fs::{AttrChange, Ext2, Ext2Error, FileKind};

use super::{volume_error, OsFile};
use crate::os_layout::MANIFEST_PATH;
use crate::os_manifest::Manifest;

/// The trees only the image build writes: a session cannot write `/system`
/// (the kernel refuses it), and `/docs/os` is the build's half of the docs
/// ([`fhs::docs`]), `pkgd`'s being `/docs/apps`. `/system/etc` holds seeds the
/// running system owns, so it is not on the list; `/apps`, `/conf`, `/logs`,
/// `/home` and `/data` are state and never are.
pub(super) fn build_owned_roots() -> [&'static str; 5] {
    [
        fhs::SYSTEM_BIN,
        fhs::SYSTEM_LIB,
        fhs::SYSTEM_SHARE,
        fhs::SYSTEM_PACKAGES,
        fhs::docs::OS_DOCS,
    ]
}

/// Whether `error` (a [`volume_error`] text) is the volume running out of room.
pub(super) fn is_no_space(error: &str) -> bool {
    error.contains("NoSpace")
}

/// The manifest to keep on the volume while an update writes: everything the
/// old build placed and everything the new one will. A build that dies after
/// this point leaves nothing the next update cannot find (an entry that does
/// not exist is skipped when it is removed).
pub(super) fn with_new(old: &Manifest, new: &Manifest) -> Manifest {
    let mut entries = old.entries.clone();
    entries.extend(new.entries.iter().map(|(path, kind)| (path.clone(), *kind)));
    Manifest { entries }
}

/// Replace `/system/.image-manifest` with `manifest` and flush it. The new
/// text is written beside it and renamed over it, so a crash leaves the old
/// manifest or the new one, never an empty file (which `plan` would take for
/// a valid build that placed nothing).
pub(super) fn write_manifest(volume: &Ext2, manifest: &Manifest, stamp: i64) -> Result<(), String> {
    let next = format!("{MANIFEST_PATH}.new");
    volume
        .write_file(&next, manifest.to_text().as_bytes(), 0o644, 0, 0, stamp)
        .map_err(|e| volume_error("write the manifest", e))?;
    volume
        .rename(&next, MANIFEST_PATH)
        .map_err(|e| volume_error("replace the manifest", e))?;
    volume.flush().map_err(|e| volume_error("flush", e))
}

/// Whether `file` is on `volume` exactly as it would be written: same bytes.
/// `bytes` is its content. The mode and owner are not compared; the caller
/// sets them.
pub(super) fn has_content(volume: &Ext2, file: &OsFile, bytes: &[u8]) -> bool {
    let Ok(meta) = volume.lookup(&file.path) else {
        return false;
    };
    meta.kind == FileKind::File
        && meta.size == bytes.len() as u64
        && volume
            .read_file(&file.path)
            .is_ok_and(|current| current == bytes)
}

/// Give an existing file the mode and owner of `file` when it has others.
pub(super) fn set_placement(volume: &Ext2, file: &OsFile) -> Result<(), String> {
    let meta = volume
        .lookup(&file.path)
        .map_err(|e| volume_error(&format!("stat {}", file.path), e))?;
    let wanted = file.placement;
    if meta.mode & 0o7777 == file.mode && meta.uid == wanted.uid && meta.gid == wanted.gid {
        return Ok(());
    }
    let change = AttrChange {
        mode: Some(file.mode),
        uid: Some(wanted.uid),
        gid: Some(wanted.gid),
        ..AttrChange::default()
    };
    volume
        .setattr(&file.path, &change)
        .map(|_| ())
        .map_err(|e| volume_error(&format!("chmod {}", file.path), e))
}

/// What [`reclaim_leftovers`] freed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Reclaimed {
    pub files: usize,
    pub bytes: u64,
}

/// Remove the files of the build-owned trees ([`build_owned_roots`]) that
/// neither manifest lists and the new build does not place: what an
/// interrupted update left (a feature switched off since, a path renamed).
/// Directories they empty are removed. Anything outside those trees is state
/// and stays, as does every path in `keep`.
pub(super) fn reclaim_leftovers(
    volume: &Ext2,
    keep: &BTreeSet<String>,
) -> Result<Reclaimed, String> {
    let mut freed = Reclaimed::default();
    for root in build_owned_roots() {
        sweep(volume, root, keep, &mut freed, 0)?;
    }
    Ok(freed)
}

/// Deeper than any tree the build places; a loop in a damaged volume ends here.
const MAX_DEPTH: usize = 16;

fn sweep(
    volume: &Ext2,
    dir: &str,
    keep: &BTreeSet<String>,
    freed: &mut Reclaimed,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries = match volume.readdir(dir) {
        Ok(entries) => entries,
        Err(Ext2Error::NotFound | Ext2Error::NotDir) => return Ok(()),
        Err(error) => return Err(volume_error(&format!("list {dir}"), error)),
    };
    for entry in entries {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        let path = format!("{dir}/{}", entry.name);
        match entry.kind {
            FileKind::Dir => {
                sweep(volume, &path, keep, freed, depth + 1)?;
                if !keep.contains(&path) {
                    // Still holding something that stays: not empty, so kept.
                    let _ = volume.rmdir(&path);
                }
            }
            FileKind::File if keep.contains(&path) => {}
            FileKind::File => {
                let size = volume.lookup(&path).map_or(0, |meta| meta.size);
                match volume.unlink(&path) {
                    Ok(()) => {
                        freed.files += 1;
                        freed.bytes += size;
                    }
                    Err(Ext2Error::NotFound) => {}
                    Err(error) => return Err(volume_error(&format!("remove {path}"), error)),
                }
            }
        }
    }
    Ok(())
}

/// Every path an update may leave alone: both manifests' files and every file
/// of the build (seeds included, which no manifest lists).
pub(super) fn keep_set(old: &Manifest, new: &Manifest, files: &[OsFile]) -> BTreeSet<String> {
    let mut keep: BTreeSet<String> = old
        .entries
        .keys()
        .chain(new.entries.keys())
        .cloned()
        .collect();
    keep.extend(files.iter().map(|file| file.path.clone()));
    keep
}

/// The error of an update the volume had no room for, with what is on it and
/// what to do. `volume` is asked for its capacity; `wanted` is the size of the
/// files the build places.
pub(super) fn full_message(volume: &Ext2, error: &str, wanted: u64, hint: &str) -> String {
    let capacity = volume
        .statfs()
        .map(|stats| {
            let block = u64::from(stats.block_size);
            format!(
                "{} MiB of {} MiB free",
                (stats.blocks_free * block) >> 20,
                (stats.blocks * block) >> 20
            )
        })
        .unwrap_or_else(|_| "its capacity is unreadable".to_string());
    format!(
        "the OS volume is full ({capacity}) and this build places about {} MiB of files: \
         {error}; {hint}",
        wanted >> 20
    )
}

/// The size in bytes of what `files` place, for [`full_message`].
pub(super) fn total_bytes(files: &[OsFile]) -> u64 {
    files
        .iter()
        .map(|file| file.source.len().unwrap_or(0))
        .sum()
}
