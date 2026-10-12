//! The system and home volumes (step 5): the stick's OS volume turned back
//! into the build's directory table and file list, then written by the
//! build's own `write_volume`, so an installed volume has the layout, modes,
//! owners and manifest a built one has.

use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Arc;

use ext2fs::{Ext2, FileKind, Geometry};

use crate::os_disk::{FileIo, SECTOR};
use crate::os_image::{self, write_volume, Kind, Manifest, OsFile, Placement, Source, VolumeFile};
use crate::os_layout::{self, DirSpec, MANIFEST_PATH};
use crate::{os_recover, usb_stick};

/// The label every OS volume carries.
pub const SYSTEM_LABEL: &str = "lazyos";

/// A partition (or a window of an image file) as a block device.
#[derive(Clone, Debug)]
pub struct Extent {
    pub path: std::path::PathBuf,
    pub start_lba: u64,
    pub sectors: u64,
}

impl Extent {
    pub fn io(&self, writable: bool) -> Result<FileIo, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(&self.path)
            .map_err(|e| format!("open {}: {e}", self.path.display()))?;
        Ok(FileIo::new(file, self.start_lba, self.sectors, writable))
    }

    pub fn bytes(&self) -> u64 {
        self.sectors * SECTOR
    }
}

/// The directory table and file list that rebuild `source` (the stick's OS
/// volume): the manifest's directories and files as the build placed them,
/// every other file as a seed, the account homes as seed directories.
/// Checked: the manifest they produce is the source's own.
pub fn tree(source: &Arc<Ext2>) -> Result<(Vec<DirSpec>, Vec<OsFile>), String> {
    let text = source
        .read_file(MANIFEST_PATH)
        .map_err(|e| format!("the stick's OS volume has no {MANIFEST_PATH}: {e:?}"))?;
    let text =
        String::from_utf8(text).map_err(|_| "the stick's manifest is not UTF-8".to_string())?;
    let manifest = Manifest::parse(&text)?;
    let passwd = source.read_file(fhs::etc::PASSWD).unwrap_or_default();
    let accounts = os_layout::parse_passwd(&String::from_utf8_lossy(&passwd));
    let homes: Vec<String> = accounts
        .iter()
        .filter(|account| account.home == fhs::home_of(&account.name))
        .map(|account| account.home.clone())
        .collect();
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut pending = vec![String::from("/")];
    while let Some(dir) = pending.pop() {
        let entries = source
            .readdir(&dir)
            .map_err(|e| format!("list {dir}: {e:?}"))?;
        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let path = if dir == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{dir}/{}", entry.name)
            };
            if path == "/lost+found" || path == MANIFEST_PATH {
                continue;
            }
            let meta = source
                .lookup(&path)
                .map_err(|e| format!("stat {path}: {e:?}"))?;
            let mode = meta.mode & 0o7777;
            let listed = manifest.entries.get(&path).copied();
            match entry.kind {
                FileKind::Dir => {
                    if listed == Some(Kind::Dir) {
                        let spec = match homes.contains(&path) {
                            true => DirSpec::seed(&path, mode, meta.uid, meta.gid),
                            false => DirSpec::new(&path, mode, meta.uid, meta.gid),
                        };
                        dirs.push(spec);
                    }
                    pending.push(path);
                }
                FileKind::File => {
                    let placement = match listed {
                        Some(Kind::File) => Placement::owned(meta.uid, meta.gid),
                        _ => Placement::seed(meta.uid, meta.gid),
                    };
                    let source = Source::Volume(VolumeFile {
                        volume: Arc::clone(source),
                        path: path.clone(),
                    });
                    files.push(OsFile {
                        path,
                        source,
                        mode,
                        placement,
                    });
                }
            }
        }
    }
    // Parents before children, as the build's table lists them.
    dirs.sort_by(|a, b| a.path.cmp(&b.path));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let rebuilt = Manifest::of(&dirs, &files)?;
    if rebuilt != manifest {
        let missing = manifest
            .entries
            .keys()
            .find(|path| !rebuilt.entries.contains_key(*path));
        let extra = rebuilt
            .entries
            .keys()
            .find(|path| !manifest.entries.contains_key(*path));
        return Err(format!(
            "the stick's OS volume does not match its manifest (missing {missing:?}, unlisted {extra:?})"
        ));
    }
    Ok((dirs, files))
}

/// The accounts of the stick's `/system/etc/passwd`, for the home volume.
pub fn accounts(source: &Ext2) -> Vec<os_layout::Account> {
    let passwd = source.read_file(fhs::etc::PASSWD).unwrap_or_default();
    os_layout::parse_passwd(&String::from_utf8_lossy(&passwd))
}

/// Format `extent` as a new OS volume with `uuid` and write the tree.
pub fn install_system(
    extent: &Extent,
    uuid: [u8; 16],
    dirs: &[DirSpec],
    files: &[OsFile],
) -> Result<(), String> {
    let io = extent.io(true)?;
    let stamp = os_image::now();
    ext2fs::format(
        &io,
        Geometry::for_size(extent.bytes()),
        SYSTEM_LABEL,
        uuid,
        stamp,
    )
    .map_err(|e| os_image::volume_error("format the system", e))?;
    let volume = os_image::open_cached(io)?;
    write_volume(&volume, None, dirs, files, stamp)?;
    Ok(())
}

/// Update the OS volume on `extent` in place from its own manifest: the
/// rebuild's rules (`/conf`, `/apps`, `/logs`, `/home` and anything a user
/// made survive). A volume that stopped uncleanly is checked first, and
/// damage a crash cannot leave refuses the update. Returns its UUID.
pub fn update_system(
    extent: &Extent,
    dirs: &[DirSpec],
    files: &[OsFile],
) -> Result<[u8; 16], String> {
    let mut volume = os_image::open_cached(extent.io(true)?)?;
    let text = volume
        .read_file(MANIFEST_PATH)
        .map_err(|e| format!("{} has no {MANIFEST_PATH}: {e:?}", extent.path.display()))?;
    let old = Manifest::parse(&String::from_utf8_lossy(&text))?;
    os_recover::recover(&mut volume, false)?;
    write_volume(&volume, Some(&old), dirs, files, os_image::now())?;
    Ok(volume.uuid())
}

/// Format `extent` as a home volume (label `lazyhome`) with a private
/// directory per account, as the stick's own home is made; then copy
/// `from` (another home volume) into it when given.
pub fn install_home(
    extent: &Extent,
    uuid: [u8; 16],
    accounts: &[os_layout::Account],
    from: Option<&Path>,
) -> Result<(), String> {
    usb_stick::format_home(extent.io(true)?, uuid, &usb_stick::home_dirs(accounts))?;
    let Some(from) = from else {
        return Ok(());
    };
    let source = open_any(from)?;
    let target = os_image::open_cached(extent.io(true)?)?;
    copy_tree(&source, &target, "/")?;
    target
        .flush()
        .map_err(|e| os_image::volume_error("flush home", e))
}

/// An ext2 volume in a file or on a device: the whole file, or the last
/// partition of an MBR stick image (the home partition is always last).
fn open_any(path: &Path) -> Result<Ext2, String> {
    let length = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    let whole = Extent {
        path: path.to_path_buf(),
        start_lba: 0,
        sectors: length / SECTOR,
    };
    if let Ok(volume) = Ext2::open(Box::new(whole.io(false)?), os_image::now) {
        return Ok(volume);
    }
    let mut mbr = [0u8; 512];
    std::io::Read::read_exact(
        &mut std::fs::File::open(path).map_err(|e| e.to_string())?,
        &mut mbr,
    )
    .map_err(|e| e.to_string())?;
    let last = (1..=4)
        .rev()
        .find_map(|n| crate::os_disk::mbr_entry(&mbr, n).filter(|(kind, _, _)| *kind != 0))
        .ok_or_else(|| {
            format!(
                "{} is neither an ext2 volume nor a partitioned image",
                path.display()
            )
        })?;
    let part = Extent {
        path: path.to_path_buf(),
        start_lba: last.1,
        sectors: last.2,
    };
    Ext2::open(Box::new(part.io(false)?), os_image::now)
        .map_err(|e| format!("{}: not ext2 ({e:?})", path.display()))
}

/// Copy every directory and file under `dir` of `source` into `target`,
/// keeping modes and owners; what `target` already has is overwritten.
fn copy_tree(source: &Ext2, target: &Ext2, dir: &str) -> Result<(), String> {
    for entry in source
        .readdir(dir)
        .map_err(|e| format!("list {dir}: {e:?}"))?
    {
        if entry.name == "." || entry.name == ".." || (dir == "/" && entry.name == "lost+found") {
            continue;
        }
        let path = if dir == "/" {
            format!("/{}", entry.name)
        } else {
            format!("{dir}/{}", entry.name)
        };
        let meta = source
            .lookup(&path)
            .map_err(|e| format!("stat {path}: {e:?}"))?;
        let mode = meta.mode & 0o7777;
        match entry.kind {
            FileKind::Dir => {
                target
                    .mkdir_p(&path, mode, meta.uid, meta.gid)
                    .map_err(|e| os_image::volume_error(&format!("mkdir {path}"), e))?;
                let change = ext2fs::AttrChange {
                    mode: Some(mode),
                    uid: Some(meta.uid),
                    gid: Some(meta.gid),
                    ..Default::default()
                };
                target
                    .setattr(&path, &change)
                    .map_err(|e| os_image::volume_error(&format!("chmod {path}"), e))?;
                copy_tree(source, target, &path)?;
            }
            FileKind::File => {
                let bytes = source
                    .read_file(&path)
                    .map_err(|e| format!("read {path}: {e:?}"))?;
                target
                    .write_file(&path, &bytes, mode, meta.uid, meta.gid, meta.times.mtime)
                    .map_err(|e| os_image::volume_error(&format!("write {path}"), e))?;
            }
        }
    }
    Ok(())
}

/// What an existing ext2 volume on `extent` is: its UUID, label and whether
/// it is a LazyOS system; `None` when it does not open.
pub fn identify(extent: &Extent) -> Option<([u8; 16], String, bool)> {
    let volume = Ext2::open(Box::new(extent.io(false).ok()?), os_image::now).ok()?;
    let label = volume
        .label()
        .iter()
        .take_while(|b| **b != 0)
        .map(|&b| b as char)
        .collect();
    let system = volume.lookup(MANIFEST_PATH).is_ok();
    Some((volume.uuid(), label, system))
}
