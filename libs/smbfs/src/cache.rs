//! What the daemon believes about the share, for [`FRESH_TICKS`]: each
//! path's attributes and each directory's listing. A share has other
//! clients, so nothing is kept longer than the kernel keeps it
//! (`fs::fuse::ATTR_TICKS`); this daemon's own changes update the entries in
//! place instead of dropping them.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use smbwire::msg::{DirEntry, FileInfo};

use crate::{join, split, FRESH_TICKS};

/// One name of a listing and whether it is a directory.
pub type Listed = (String, bool);

#[derive(Default)]
pub struct Cache {
    meta: BTreeMap<String, (FileInfo, u64)>,
    listings: BTreeMap<String, (Vec<Listed>, u64)>,
}

fn fresh(at: u64, now: u64) -> bool {
    now < at.saturating_add(FRESH_TICKS)
}

/// Whether `path` is `top` or below it.
fn under(path: &str, top: &str) -> bool {
    top.is_empty()
        || path == top
        || (path.starts_with(top) && path.as_bytes().get(top.len()) == Some(&b'/'))
}

impl Cache {
    pub fn get(&self, path: &str, now: u64) -> Option<FileInfo> {
        let (info, at) = self.meta.get(path)?;
        fresh(*at, now).then_some(*info)
    }

    pub fn put(&mut self, path: &str, info: FileInfo, now: u64) {
        self.meta.insert(String::from(path), (info, now));
    }

    pub fn listing(&self, dir: &str, now: u64) -> Option<&Vec<Listed>> {
        let (names, at) = self.listings.get(dir)?;
        fresh(*at, now).then_some(names)
    }

    /// A fresh listing of `dir`: its names, and every entry's attributes.
    pub fn put_listing(&mut self, dir: &str, entries: &[DirEntry], now: u64) {
        let names = entries
            .iter()
            .map(|e| (e.name.clone(), e.info.is_dir()))
            .collect();
        for entry in entries {
            self.put(&join(dir, &entry.name), entry.info, now);
        }
        self.listings.insert(String::from(dir), (names, now));
    }

    /// This daemon made `path` (`info` is what the server answered).
    pub fn added(&mut self, path: &str, info: FileInfo, now: u64) {
        let (dir, name) = split(path);
        if let Some((names, _)) = self.listings.get_mut(dir) {
            names.retain(|(n, _)| n != name);
            names.push((String::from(name), info.is_dir()));
        }
        self.put(path, info, now);
    }

    /// This daemon removed `path`: it, everything below it and its name in
    /// the parent's listing go.
    pub fn removed(&mut self, path: &str) {
        let (dir, name) = split(path);
        if let Some((names, _)) = self.listings.get_mut(dir) {
            names.retain(|(n, _)| n != name);
        }
        self.meta.retain(|p, _| !under(p, path));
        self.listings.retain(|p, _| !under(p, path));
    }

    /// Forget `dir`'s listing (a rename may have put a name in it).
    pub fn drop_listing(&mut self, dir: &str) {
        self.listings.remove(dir);
    }

    /// A write or truncate made `path` `size` bytes long (or, with `grow`,
    /// at least that).
    pub fn resized(&mut self, path: &str, size: u64, grow: bool) {
        if let Some((info, _)) = self.meta.get_mut(path) {
            info.end_of_file = if grow {
                info.end_of_file.max(size)
            } else {
                size
            };
        }
    }

    /// Forget everything (the session was lost: the server may have moved on).
    pub fn clear(&mut self) {
        self.meta.clear();
        self.listings.clear();
    }
}
