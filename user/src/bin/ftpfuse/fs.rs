//! [`FtpFs`]: an FTP server's tree as a [`FuseFs`].
//!
//! Metadata comes from directory listings, kept for [`LISTING_TICKS`] and
//! updated in place by this daemon's own changes. FTP has no random-access
//! write, so writes are mapped onto what it has:
//!
//! * a write at the end of a file is an `APPE` (an empty file's first write
//!   a `STOR`): a sequential `cp` or `>>` costs one upload per 64 KiB request;
//! * any other write, and a truncate to a size other than 0 or the current
//!   one, reads the file whole, patches it and `STOR`s it back (files up to
//!   [`MAX_FILE`]).
//!
//! Reads fetch a file whole with `RETR` on first use and serve slices of it
//! until the file changes. Attribute changes are accepted and ignored (FTP has
//! no portable `chmod`), so `touch` and `cp -p` work.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use ftpwire::listing::{parse_listing, ListEntry};
use fused::daemon::{Errno, FuseFs, Target};
use fused::inodes::Inodes;
use fused::payload::{DirEnt, SetAttrRecord, StatFsRecord};
use fused::wire::{errno, Attr, S_IFDIR, S_IFREG};
use user::sys;

use crate::link::{remote, Link, RenameError, MAX_FILE};

/// How long a listing is believed, ticks (100 Hz).
const LISTING_TICKS: u64 = 300;
/// Whole files kept for reading, and the bytes they may hold together: two
/// files read in alternation (`cmp a b`, `diff`) must not fetch each other
/// out.
const CACHED_FILES: usize = 4;
const CACHED_BYTES: usize = 48 * 1024 * 1024;
/// The `statfs` magic `ftpfuse` reports ("FTPF").
const MAGIC: u64 = 0x4654_5046;

struct Listing {
    entries: Vec<ListEntry>,
    fetched: u64,
}

pub struct FtpFs {
    link: Link,
    inodes: Inodes,
    dirs: BTreeMap<String, Listing>,
    /// Files read whole, least recently used first: path and bytes.
    files: Vec<(String, Vec<u8>)>,
    mlsd: Option<bool>,
    uid: u32,
    gid: u32,
    started: i64,
}

/// The parent directory and last name of a non-root path.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        String::from(name)
    } else {
        alloc::format!("{dir}/{name}")
    }
}

impl FtpFs {
    pub fn new(link: Link, uid: u32, gid: u32, started: i64) -> FtpFs {
        FtpFs {
            link,
            inodes: Inodes::new(),
            dirs: BTreeMap::new(),
            files: Vec::new(),
            mlsd: None,
            uid,
            gid,
            started,
        }
    }

    /// Keep an idle control connection open (servers drop silent ones).
    pub fn keepalive(&mut self) {
        if self.link.connected() && self.link.command("NOOP", None, &[2]).is_err() {
            self.link.drop_connection();
        }
    }

    fn path_of<'a>(&'a self, target: Target<'a>) -> Result<String, Errno> {
        match target {
            Target::Path(path) => Ok(String::from(path)),
            Target::Node { ino, generation: 1 } => {
                self.inodes.path(ino).map(String::from).ok_or(errno::ESTALE)
            }
            Target::Node { .. } => Err(errno::ESTALE),
        }
    }

    /// The entries of directory `dir`, fetched when not fresh.
    fn listing(&mut self, dir: &str) -> Result<&mut Vec<ListEntry>, Errno> {
        let now = sys::clock();
        let fresh = self
            .dirs
            .get(dir)
            .is_some_and(|l| now < l.fetched + LISTING_TICKS);
        if !fresh {
            let (body, mlsd) = self.link.listing(&remote(dir), &mut self.mlsd)?;
            let entries = parse_listing(&body, mlsd);
            self.dirs.insert(
                String::from(dir),
                Listing {
                    entries,
                    fetched: now,
                },
            );
        }
        Ok(&mut self.dirs.get_mut(dir).expect("just listed").entries)
    }

    /// The listing entry of a non-root `path`.
    fn entry(&mut self, path: &str) -> Result<ListEntry, Errno> {
        let (dir, name) = split(path);
        if !dir.is_empty() && !self.entry(dir)?.dir {
            return Err(errno::ENOTDIR);
        }
        self.listing(dir)?
            .iter()
            .find(|e| e.name == name)
            .cloned()
            .ok_or(errno::ENOENT)
    }

    fn attr(&mut self, path: &str, entry: Option<&ListEntry>) -> Attr {
        let (dir, size, mtime) = entry.map_or((true, 0, None), |e| (e.dir, e.size, e.mtime));
        let time = mtime.unwrap_or(self.started);
        Attr {
            ino: self.inodes.ino(path),
            generation: 1,
            mode: if dir {
                S_IFDIR | 0o755
            } else {
                S_IFREG | 0o644
            },
            uid: u64::from(self.uid),
            gid: u64::from(self.gid),
            size,
            atime: time,
            mtime: time,
            ctime: time,
        }
    }

    fn attr_of(&mut self, path: &str) -> Result<Attr, Errno> {
        if path.is_empty() {
            return Ok(self.attr(path, None));
        }
        let entry = self.entry(path)?;
        Ok(self.attr(path, Some(&entry)))
    }

    fn file_entry(&mut self, path: &str) -> Result<ListEntry, Errno> {
        if path.is_empty() {
            return Err(errno::EISDIR);
        }
        let entry = self.entry(path)?;
        if entry.dir {
            return Err(errno::EISDIR);
        }
        Ok(entry)
    }

    /// The whole file, from the cache when it still has the listed size.
    fn contents(&mut self, path: &str, size: u64) -> Result<&mut Vec<u8>, Errno> {
        let hit = self
            .files
            .iter()
            .position(|(p, bytes)| p == path && bytes.len() as u64 == size);
        match hit {
            // Most recently used goes last.
            Some(index) => {
                let entry = self.files.remove(index);
                self.files.push(entry);
            }
            None => {
                if size > MAX_FILE as u64 {
                    return Err(errno::EOPNOTSUPP);
                }
                let bytes = self.link.fetch("RETR", &remote(path), MAX_FILE)?;
                self.set_size(path, bytes.len() as u64);
                self.cache(path, bytes);
            }
        }
        Ok(&mut self.files.last_mut().expect("just cached").1)
    }

    /// Keep `bytes` as `path`'s cached copy, evicting the least recently used
    /// files past [`CACHED_FILES`] or [`CACHED_BYTES`] (never the new one).
    fn cache(&mut self, path: &str, bytes: Vec<u8>) {
        self.files.retain(|(p, _)| p != path);
        self.files.push((String::from(path), bytes));
        let total = |files: &[(String, Vec<u8>)]| files.iter().map(|(_, b)| b.len()).sum::<usize>();
        while self.files.len() > 1
            && (self.files.len() > CACHED_FILES || total(&self.files) > CACHED_BYTES)
        {
            self.files.remove(0);
        }
    }

    /// Record `path`'s new size in its parent's listing (adding it if new).
    fn set_size(&mut self, path: &str, size: u64) {
        let (dir, name) = split(path);
        if let Some(listing) = self.dirs.get_mut(dir) {
            match listing.entries.iter_mut().find(|e| e.name == name) {
                Some(entry) => entry.size = size,
                None => listing.entries.push(ListEntry {
                    name: String::from(name),
                    dir: false,
                    size,
                    mtime: None,
                }),
            }
        }
    }

    /// Store `bytes` as the whole of `path`, keeping them as the cached copy.
    fn rewrite(&mut self, path: &str, bytes: Vec<u8>) -> Result<(), Errno> {
        self.link.store("STOR", &remote(path), &bytes)?;
        self.set_size(path, bytes.len() as u64);
        self.cache(path, bytes);
        Ok(())
    }

    /// Forget what is cached about `path` and below it.
    fn forget(&mut self, path: &str) {
        let (dir, name) = split(path);
        if let Some(listing) = self.dirs.get_mut(dir) {
            listing.entries.retain(|e| e.name != name);
        }
        let below = alloc::format!("{path}/");
        self.dirs.retain(|d, _| d != path && !d.starts_with(&below));
        self.files
            .retain(|(p, _)| p != path && !p.starts_with(&below));
    }

    /// `path` may have changed on the server in a way not known here: drop
    /// its cached bytes and its parent's listing, so both are read again.
    fn unsure(&mut self, path: &str) {
        let (dir, _) = split(path);
        self.dirs.remove(dir);
        self.files.retain(|(p, _)| p != path);
    }

    /// The parent of a new `path` exists and the name is free.
    fn creatable(&mut self, path: &str) -> Result<(), Errno> {
        match self.entry(path) {
            Ok(_) => Err(errno::EEXIST),
            Err(errno::ENOENT) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn add_entry(&mut self, path: &str, dir: bool) -> Attr {
        let (parent, name) = split(path);
        let entry = ListEntry {
            name: String::from(name),
            dir,
            size: 0,
            mtime: None,
        };
        if let Some(listing) = self.dirs.get_mut(parent) {
            listing.entries.push(entry.clone());
        }
        self.attr(path, Some(&entry))
    }
}

impl FuseFs for FtpFs {
    fn lookup(&mut self, target: Target) -> Result<Attr, Errno> {
        let path = self.path_of(target)?;
        self.attr_of(&path)
    }

    fn read(&mut self, target: Target, offset: u64, out: &mut [u8]) -> Result<usize, Errno> {
        let path = self.path_of(target)?;
        let entry = self.file_entry(&path)?;
        let bytes = self.contents(&path, entry.size)?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let count = out.len().min(bytes.len() - start);
        out[..count].copy_from_slice(&bytes[start..start + count]);
        Ok(count)
    }

    fn write(&mut self, target: Target, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        let path = self.path_of(target)?;
        let size = self.file_entry(&path)?.size;
        let end = offset.checked_add(data.len() as u64).ok_or(errno::EINVAL)?;
        if end > MAX_FILE as u64 {
            return Err(errno::ENOSPC);
        }
        if offset == size {
            let verb = if size == 0 { "STOR" } else { "APPE" };
            match self.link.store(verb, &remote(&path), data) {
                Ok(()) => {
                    // Keep the cached copy in step instead of fetching it again.
                    if let Some((_, bytes)) = self.files.iter_mut().find(|(p, _)| *p == path) {
                        bytes.truncate(size as usize);
                        bytes.extend_from_slice(data);
                    }
                    self.set_size(&path, end);
                    return Ok(data.len());
                }
                // A server without APPE: rewrite instead.
                Err(errno::EOPNOTSUPP) if verb == "APPE" => {}
                Err(error) => {
                    // The server may hold part of the data: what is cached
                    // about the file can no longer be believed.
                    self.unsure(&path);
                    return Err(error);
                }
            }
        }
        let mut bytes = core::mem::take(self.contents(&path, size)?);
        if bytes.len() < end as usize {
            bytes.resize(end as usize, 0);
        }
        bytes[offset as usize..end as usize].copy_from_slice(data);
        self.rewrite(&path, bytes)?;
        Ok(data.len())
    }

    fn truncate(&mut self, target: Target, size: u64) -> Result<(), Errno> {
        let path = self.path_of(target)?;
        let current = self.file_entry(&path)?.size;
        if size == current {
            return Ok(());
        }
        if size > MAX_FILE as u64 {
            return Err(errno::ENOSPC);
        }
        let mut bytes = if size == 0 {
            Vec::new()
        } else {
            core::mem::take(self.contents(&path, current)?)
        };
        bytes.resize(size as usize, 0);
        self.rewrite(&path, bytes)
    }

    fn setattr(&mut self, path: &str, _change: &SetAttrRecord) -> Result<Attr, Errno> {
        self.attr_of(path)
    }

    fn create(&mut self, path: &str, _mode: u16, _uid: u32, _gid: u32) -> Result<Attr, Errno> {
        self.creatable(path)?;
        self.link.store("STOR", &remote(path), &[])?;
        Ok(self.add_entry(path, false))
    }

    fn mkdir(&mut self, path: &str, _mode: u16, _uid: u32, _gid: u32) -> Result<Attr, Errno> {
        self.creatable(path)?;
        self.link.command("MKD", Some(&remote(path)), &[2])?;
        Ok(self.add_entry(path, true))
    }

    fn unlink(&mut self, path: &str) -> Result<(), Errno> {
        self.file_entry(path)?;
        self.link.command("DELE", Some(&remote(path)), &[2])?;
        self.forget(path);
        self.inodes.forget(path);
        Ok(())
    }

    fn rmdir(&mut self, path: &str) -> Result<(), Errno> {
        if path.is_empty() {
            return Err(errno::EPERM);
        }
        if !self.entry(path)?.dir {
            return Err(errno::ENOTDIR);
        }
        if !self.listing(path)?.is_empty() {
            return Err(errno::ENOTEMPTY);
        }
        self.link.command("RMD", Some(&remote(path)), &[2])?;
        self.forget(path);
        self.inodes.forget(path);
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Errno> {
        if from.is_empty() || to.is_empty() {
            return Err(errno::EPERM);
        }
        let moving = self.entry(from)?;
        if from == to {
            return Ok(());
        }
        let replaces = match self.entry(to) {
            Ok(existing) if !existing.dir && !moving.dir => true,
            Ok(_) => return Err(errno::EEXIST),
            Err(errno::ENOENT) => false,
            Err(error) => return Err(error),
        };
        // A file over a file replaces it, as POSIX says, and a failed rename
        // must leave the destination alone. Servers differ on whether RNTO may
        // overwrite: try it first, and only when RNTO was refused with a
        // file in the way, delete that file and try once more.
        match self.link.rename(&remote(from), &remote(to)) {
            Ok(()) => {}
            Err(RenameError::Target(_)) if replaces => {
                self.unlink(to)?;
                self.link
                    .rename(&remote(from), &remote(to))
                    .map_err(RenameError::errno)?;
            }
            Err(error) => return Err(error.errno()),
        }
        self.forget(from);
        // An RNTO that overwrote `to` leaves its old bytes cached under that
        // name; a same-size replacement would otherwise read back stale.
        self.forget(to);
        let (parent, _) = split(to);
        self.dirs.remove(parent);
        self.inodes.rename(from, to);
        Ok(())
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<DirEnt>, Errno> {
        if !path.is_empty() && !self.entry(path)?.dir {
            return Err(errno::ENOTDIR);
        }
        let entries = self.listing(path)?.clone();
        Ok(entries
            .into_iter()
            .map(|e| DirEnt {
                ino: self.inodes.ino(&join(path, &e.name)),
                dir: e.dir,
                name: e.name,
            })
            .collect())
    }

    fn statfs(&mut self) -> Result<StatFsRecord, Errno> {
        // FTP reports no capacity: a large, constant figure.
        Ok(StatFsRecord {
            magic: MAGIC,
            block_size: 4096,
            blocks: 1 << 28,
            blocks_free: 1 << 27,
            files: 1 << 20,
            files_free: 1 << 19,
            name_max: ftpwire::listing::MAX_NAME as u64,
        })
    }
}
