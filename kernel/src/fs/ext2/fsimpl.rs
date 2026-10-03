//! The [`Filesystem`] impl over the library: path in, library call, errors and
//! metadata converted to the VFS types. Every call holds the volume's gate
//! ([`Ext2`]'s `gate`) for its whole length.

use alloc::vec::Vec;

use ext2fs::{AttrChange, FsStats, InodeMeta, Owner};

use super::{hidden, Ext2};
use crate::fs::vfs::{DirEntry, FileKind, Filesystem, FsError, Id, Meta, SetAttr, StatFs, Times};

/// Bytes one library read or write moves before the next piece. A syscall
/// runs with interrupts off, and a 1 MiB write into the block cache takes
/// about 40 ms of CPU, long enough for the keyboard controller's queue to
/// overflow (`input::ps2`); between pieces the controller is drained, so the
/// stretch without input servicing stays near 2 ms whatever the size.
const PIECE: usize = 64 * 1024;

impl Filesystem for Ext2 {
    fn name(&self) -> &'static str {
        // A volume on a device that cannot be written is mounted read-only;
        // `/proc/mounts` reports the mode from this name.
        if self.volume.is_read_only() {
            "ext2 (ro)"
        } else {
            "ext2 (rw)"
        }
    }

    fn lookup(&self, path: &str) -> Result<Meta, FsError> {
        let _gate = self.gate.lock();
        Ok(meta(self.volume.lookup(path)?))
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let _gate = self.gate.lock();
        let mut done = 0;
        for piece in buf.chunks_mut(PIECE) {
            crate::input::ps2::service();
            let read = self.volume.read(path, offset + done as u64, piece)?;
            done += read;
            if read < piece.len() {
                break;
            }
        }
        Ok(done)
    }

    fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let _gate = self.gate.lock();
        let mut done = 0;
        for piece in data.chunks(PIECE) {
            crate::input::ps2::service();
            let written = self.volume.write(path, offset + done as u64, piece)?;
            done += written;
            if written < piece.len() {
                break;
            }
        }
        Ok(done)
    }

    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        let _gate = self.gate.lock();
        Ok(self.volume.truncate(path, size)?)
    }

    fn setattr(&self, path: &str, attr: &SetAttr) -> Result<Meta, FsError> {
        let _gate = self.gate.lock();
        let change = AttrChange {
            mode: attr.mode,
            uid: attr.uid,
            gid: attr.gid,
            atime: attr.atime,
            mtime: attr.mtime,
            ctime: attr.ctime,
        };
        Ok(meta(self.volume.setattr(path, &change)?))
    }

    fn create(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let _gate = self.gate.lock();
        Ok(meta(self.volume.create(path, mode, owner_of(owner))?))
    }

    fn mkdir(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let _gate = self.gate.lock();
        Ok(meta(self.volume.mkdir(path, mode, owner_of(owner))?))
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        let _gate = self.gate.lock();
        // A parked orphan (a reserved name) is deleted inode-first so a stop
        // part-way can be resumed by the next mount's reclaim.
        let name = path.trim_matches('/').rsplit('/').next().unwrap_or("");
        if hidden::is_reserved(name) {
            Ok(self.volume.unlink_parked(path)?)
        } else {
            Ok(self.volume.unlink(path)?)
        }
    }

    fn rmdir(&self, path: &str) -> Result<(), FsError> {
        let _gate = self.gate.lock();
        Ok(self.volume.rmdir(path)?)
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        let _gate = self.gate.lock();
        Ok(self.volume.rename(from, to)?)
    }

    fn flush(&self) -> Result<(), FsError> {
        let _gate = self.gate.lock();
        Ok(self.volume.flush()?)
    }

    fn writeback(&self, pressure: bool) -> Result<(), FsError> {
        Ext2::writeback(self, pressure)
    }

    fn statfs(&self) -> Result<StatFs, FsError> {
        let _gate = self.gate.lock();
        let FsStats {
            magic,
            block_size,
            blocks,
            blocks_free,
            files,
            files_free,
            name_max,
        } = self.volume.statfs()?;
        Ok(StatFs {
            magic,
            block_size,
            blocks,
            blocks_free,
            files,
            files_free,
            name_max,
        })
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let _gate = self.gate.lock();
        Ok(self
            .volume
            .readdir(path)?
            .into_iter()
            .map(|entry| DirEntry {
                name: entry.name,
                ino: entry.ino,
                kind: kind(entry.kind),
            })
            .collect())
    }
}

fn owner_of(owner: Id) -> Owner {
    Owner {
        uid: owner.uid,
        gid: owner.gid,
    }
}

fn kind(kind: ext2fs::FileKind) -> FileKind {
    match kind {
        ext2fs::FileKind::File => FileKind::File,
        ext2fs::FileKind::Dir => FileKind::Dir,
    }
}

fn meta(meta: InodeMeta) -> Meta {
    Meta {
        ino: meta.ino,
        mode: meta.mode,
        uid: meta.uid,
        gid: meta.gid,
        size: meta.size,
        kind: kind(meta.kind),
        times: Times {
            atime: meta.times.atime,
            mtime: meta.times.mtime,
            ctime: meta.times.ctime,
        },
    }
}
