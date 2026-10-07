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
    /// Mounts currently in the table.
    pub mounts: usize,
}

/// A cached directory entry: just the inode number; the metadata lives in the
/// inode cache so a path rename does not duplicate it.
pub(super) struct Dentry {
    pub(super) ino: u64,
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
    /// (a hard link) loses the metadata, since it is keyed by inode number.
    pub fn forget(&mut self, path: &str) {
        let path = Path::parse(path);
        let Ok((mount, rel)) = self.resolve_mount(&path) else {
            return;
        };
        if let Some(dentry) = self.dentry.get(&(mount, rel)) {
            self.inodes.remove(&(mount, dentry.ino));
        }
    }

    /// Cache-aware metadata lookup for an absolute path.
    pub(super) fn stat_path(&mut self, path: &Path) -> Result<Meta, FsError> {
        let (mount, rel) = self.resolve_mount(path)?;
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
        self.inodes.insert((mount, meta.ino), meta);
        self.dentry
            .insert((mount, String::from(rel)), Dentry { ino: meta.ino });
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
