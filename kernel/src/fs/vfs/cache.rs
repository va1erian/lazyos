//! The dentry and inode caches of the [`Vfs`] (see the module docs of `vfs.rs`
//! for what they hold and when they are invalidated), and their counters.

use alloc::string::String;
use alloc::vec::Vec;

use super::{FsError, Meta, Path, Vfs};

/// Cache counters, exposed through [`Vfs::cache_stats`] for tests and future
/// `/proc` reporting.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct CacheStats {
    /// Positive dentry lookups served from the cache.
    pub dentry_hits: u64,
    /// Lookups that had to go to the filesystem.
    pub dentry_misses: u64,
    /// Inode entries found for a cached dentry.
    pub inode_hits: u64,
    /// Inode entries that had to be re-read (miss or dropped dentry).
    pub inode_misses: u64,
    /// Number of invalidation passes (one per mutation).
    pub invalidations: u64,
    /// Entries dropped because their filesystem's cache lifetime ended.
    pub expired: u64,
    /// Mounts currently in the table.
    pub mounts: usize,
}

/// A cached directory entry: just the inode number; the metadata lives in the
/// inode cache so a path rename does not duplicate it. `expires` is the
/// filesystem's [`super::Filesystem::cache_deadline`] when it was cached.
pub(super) struct Dentry {
    pub(super) ino: u64,
    pub(super) expires: Option<u64>,
}

impl Vfs {
    /// A snapshot of the cache counters.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub fn cache_stats(&self) -> CacheStats {
        self.stats
    }

    /// Drop the cached metadata for `path` (and, for a directory, everything
    /// cached below it). Mutations call this internally; it is public so a
    /// filesystem that changed behind the VFS's back can be re-read (the
    /// Linux ABI table after a native write to the same volume).
    pub fn invalidate(&mut self, path: &str) {
        let path = Path::parse(path);
        if let Ok((mount, rel)) = self.resolve_mount(&path) {
            self.invalidate_mount_path(mount, &rel);
        }
    }

    /// Drop only the cached metadata of `path`'s inode, keeping its name and
    /// everything cached below it: what a change of contents or attributes
    /// made through the other mount table needs (`chmod` on a directory does
    /// not change the names inside it). Every cached name of the inode
    /// (a hard link) loses the metadata, since it is keyed by inode number;
    /// when this name is not cached but the mount caches some inode, the
    /// inode number comes from the filesystem, so a change through one link
    /// is never answered stale through another.
    pub fn forget(&mut self, path: &str) {
        let path = Path::parse(path);
        let Ok((mount, rel)) = self.resolve_mount(&path) else {
            return;
        };
        let ino = match self.dentry.get(&(mount, rel.clone())) {
            Some(dentry) => dentry.ino,
            None if self.inodes.keys().any(|(cached, _)| *cached == mount) => {
                match self.mounts[mount].fs.stat(&rel) {
                    Ok(meta) => meta.ino,
                    Err(_) => return,
                }
            }
            None => return,
        };
        self.inodes.remove(&(mount, ino));
    }

    /// Cache-aware metadata lookup for an absolute path.
    pub(super) fn stat_path(&mut self, path: &Path) -> Result<Meta, FsError> {
        let (mount, rel) = self.resolve_mount(path)?;
        self.expire(mount, &rel);
        if let Some(dentry) = self.dentry.get(&(mount, rel.clone())) {
            self.stats.dentry_hits += 1;
            if let Some(meta) = self.inodes.get(&(mount, dentry.ino)) {
                self.stats.inode_hits += 1;
                return Ok(*meta);
            }
        } else {
            self.stats.dentry_misses += 1;
        }
        self.stats.inode_misses += 1;
        let meta = self.mounts[mount].fs.stat(&rel)?;
        self.insert_cache(mount, &rel, meta);
        Ok(meta)
    }

    /// Record a fresh metadata pair in both caches.
    pub(super) fn insert_cache(&mut self, mount: usize, rel: &str, meta: Meta) {
        let expires = self.mounts[mount].fs.cache_deadline();
        self.inodes.insert((mount, meta.ino), meta);
        self.dentry.insert(
            (mount, String::from(rel)),
            Dentry {
                ino: meta.ino,
                expires,
            },
        );
    }

    /// Drop `rel`'s cached entry once its lifetime is over, so the next
    /// lookup asks the filesystem again (a share another client changed).
    /// Only that name and its inode go: a cached descendant expires on its
    /// own clock, so it is believed no longer than any other entry, and a
    /// path through an ancestor that is no longer a directory still fails.
    fn expire(&mut self, mount: usize, rel: &str) {
        let key = (mount, String::from(rel));
        let Some(deadline) = self.dentry.get(&key).and_then(|d| d.expires) else {
            return;
        };
        if self.mounts[mount].fs.cache_now() < deadline {
            return;
        }
        if let Some(dentry) = self.dentry.remove(&key) {
            self.inodes.remove(&(mount, dentry.ino));
            self.stats.expired += 1;
        }
    }

    /// Invalidate a relative path within one mount, its inode, and any cached
    /// descendants when it names an inode that other entries still point at.
    pub(super) fn invalidate_mount_path(&mut self, mount: usize, rel: &str) {
        let ino = self
            .dentry
            .remove(&(mount, String::from(rel)))
            .map(|dentry| dentry.ino);
        if let Some(ino) = ino {
            self.inodes.remove(&(mount, ino));
            self.dentry
                .retain(|(cached_mount, _), entry| *cached_mount != mount || entry.ino != ino);
        }
        if !rel.is_empty() {
            let prefix = alloc::format!("{rel}/");
            let stale: Vec<(usize, String)> = self
                .dentry
                .keys()
                .filter(|(cached_mount, cached_rel)| {
                    *cached_mount == mount && cached_rel.starts_with(&prefix)
                })
                .cloned()
                .collect();
            for key in stale {
                if let Some(entry) = self.dentry.remove(&key) {
                    self.inodes.remove(&(mount, entry.ino));
                }
            }
        }
        self.stats.invalidations += 1;
    }
}
