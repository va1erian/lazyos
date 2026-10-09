//! [`FuseFs`] for [`SmbFs`]: each operation as SMB2 commands (the table in
//! the crate docs).

use alloc::string::String;
use alloc::vec::Vec;

use fused::daemon::{Errno, FuseFs, Target};
use fused::payload::{DirEnt, SetAttrRecord, StatFsRecord};
use fused::wire::{errno, Attr, S_IFDIR, S_IFREG};
use smbwire::client::{Open, Transport};
use smbwire::msg::{FileId, FileInfo, ATTR_DIRECTORY, ATTR_READONLY};
use smbwire::Error;

use crate::{join, split, unix_time, Connect, SmbFs, MAGIC};

impl<T: Transport, C: Connect<T>> SmbFs<T, C> {
    fn path_of(&self, target: Target) -> Result<String, Errno> {
        match target {
            Target::Path(path) => Ok(String::from(path)),
            Target::Node { ino, generation: 1 } => {
                self.inodes.path(ino).map(String::from).ok_or(errno::ESTALE)
            }
            Target::Node { .. } => Err(errno::ESTALE),
        }
    }

    fn attr(&mut self, path: &str, info: &FileInfo) -> Attr {
        let started = self.opts.started;
        let time = |filetime| unix_time(filetime).unwrap_or(started);
        let mode = if info.is_dir() {
            S_IFDIR | 0o755
        } else if info.attributes & ATTR_READONLY != 0 {
            S_IFREG | 0o444
        } else {
            S_IFREG | 0o644
        };
        Attr {
            ino: self.inodes.ino(path),
            generation: 1,
            mode,
            uid: u64::from(self.opts.uid),
            gid: u64::from(self.opts.gid),
            size: if info.is_dir() { 0 } else { info.end_of_file },
            atime: time(info.last_access),
            mtime: time(info.last_write),
            ctime: time(info.change),
        }
    }

    /// The share root's attributes, never asked for: it is a directory.
    fn root_info(&self) -> FileInfo {
        FileInfo {
            attributes: ATTR_DIRECTORY,
            ..FileInfo::default()
        }
    }

    /// `path`'s attributes: cached, absent from a fresh listing of its
    /// parent, or asked for.
    fn info_of(&mut self, path: &str) -> Result<FileInfo, Errno> {
        if path.is_empty() {
            return Ok(self.root_info());
        }
        let now = self.now();
        if let Some(info) = self.cache.get(path, now) {
            return Ok(info);
        }
        let (dir, name) = split(path);
        if let Some(names) = self.cache.listing(dir, now) {
            if !names.iter().any(|(n, _)| n == name) {
                return Err(errno::ENOENT);
            }
        }
        let info = self.run(true, |s| s.client()?.stat(path))?;
        self.cache.put(path, info, now);
        Ok(info)
    }

    fn file_info(&mut self, path: &str) -> Result<FileInfo, Errno> {
        let info = self.info_of(path)?;
        if info.is_dir() {
            return Err(errno::EISDIR);
        }
        Ok(info)
    }

    /// An open handle on the file `path`, kept for later requests; a
    /// read-only one is replaced when `write` is needed.
    fn handle(&mut self, path: &str, write: bool) -> Result<FileId, Error> {
        let now = self.now();
        if let Some(id) = self.handles.find(path, write, now) {
            return Ok(id);
        }
        let mut stale = self.handles.take_path(path);
        stale.extend(self.handles.make_room());
        self.close_all(&stale);
        let how = if write { Open::Write } else { Open::Read };
        let opened = self.client()?.open(path, how)?;
        self.cache.put(path, opened.info, now);
        self.handles.insert(path, opened.file_id, write, now);
        Ok(opened.file_id)
    }

    /// Close every kept handle of `path` and below (before it is removed or
    /// renamed).
    fn release(&mut self, path: &str) {
        let ids = self.handles.take_under(path);
        self.close_all(&ids);
    }

    fn read_at(&mut self, path: &str, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let id = self.handle(path, false)?;
        let client = self.client()?;
        let mut filled = 0;
        while filled < out.len() {
            let want = (out.len() - filled).min(client.max_read() as usize);
            let chunk = client.read(&id, offset + filled as u64, want as u32)?;
            let count = chunk.len().min(out.len() - filled);
            out[filled..filled + count].copy_from_slice(&chunk[..count]);
            filled += count;
            if chunk.len() < want {
                break;
            }
        }
        Ok(filled)
    }

    fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<usize, Error> {
        let id = self.handle(path, true)?;
        let client = self.client()?;
        let mut done = 0;
        while done < data.len() {
            let chunk = &data[done..(done + client.max_write() as usize).min(data.len())];
            let wrote = client.write(&id, offset + done as u64, chunk)? as usize;
            done += wrote;
            if wrote < chunk.len() {
                break;
            }
        }
        Ok(done)
    }

    /// A new file or directory: `CREATE`, keeping a new file open for the
    /// writes that usually follow.
    fn make(&mut self, path: &str, how: Open) -> Result<Attr, Errno> {
        let now = self.now();
        let opened = self.run(false, |s| {
            let room = s.handles.make_room();
            s.close_all(room.as_slice());
            s.client()?.open(path, how)
        })?;
        if how == Open::MakeDirectory {
            self.close_all(&[opened.file_id]);
        } else {
            self.handles.insert(path, opened.file_id, true, now);
        }
        self.cache.added(path, opened.info, now);
        Ok(self.attr(path, &opened.info))
    }

    /// Delete the file or empty directory `path`.
    fn remove(&mut self, path: &str) -> Result<(), Errno> {
        self.release(path);
        self.run(false, |s| s.client()?.delete(path))?;
        self.cache.removed(path);
        self.inodes.forget(path);
        Ok(())
    }
}

impl<T: Transport, C: Connect<T>> FuseFs for SmbFs<T, C> {
    fn lookup(&mut self, target: Target) -> Result<Attr, Errno> {
        let path = self.path_of(target)?;
        let info = self.info_of(&path)?;
        Ok(self.attr(&path, &info))
    }

    fn read(&mut self, target: Target, offset: u64, out: &mut [u8]) -> Result<usize, Errno> {
        let path = self.path_of(target)?;
        self.file_info(&path)?;
        self.run(true, |s| s.read_at(&path, offset, out))
    }

    fn write(&mut self, target: Target, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        let path = self.path_of(target)?;
        self.file_info(&path)?;
        offset.checked_add(data.len() as u64).ok_or(errno::EINVAL)?;
        let wrote = self.run(true, |s| s.write_at(&path, offset, data))?;
        self.cache.resized(&path, offset + wrote as u64, true);
        Ok(wrote)
    }

    fn truncate(&mut self, target: Target, size: u64) -> Result<(), Errno> {
        let path = self.path_of(target)?;
        self.file_info(&path)?;
        self.run(true, |s| {
            let id = s.handle(&path, true)?;
            s.client()?.set_size(&id, size)
        })?;
        self.cache.resized(&path, size, false);
        Ok(())
    }

    fn setattr(&mut self, path: &str, _change: &SetAttrRecord) -> Result<Attr, Errno> {
        // SMB has no Unix owner or mode: accepted, not applied (crate docs).
        let info = self.info_of(path)?;
        Ok(self.attr(path, &info))
    }

    fn create(&mut self, path: &str, _mode: u16, _uid: u32, _gid: u32) -> Result<Attr, Errno> {
        self.make(path, Open::Create)
    }

    fn mkdir(&mut self, path: &str, _mode: u16, _uid: u32, _gid: u32) -> Result<Attr, Errno> {
        self.make(path, Open::MakeDirectory)
    }

    fn unlink(&mut self, path: &str) -> Result<(), Errno> {
        if path.is_empty() {
            return Err(errno::EISDIR);
        }
        self.file_info(path)?;
        self.remove(path)
    }

    fn rmdir(&mut self, path: &str) -> Result<(), Errno> {
        if path.is_empty() {
            return Err(errno::EPERM);
        }
        if !self.info_of(path)?.is_dir() {
            return Err(errno::ENOTDIR);
        }
        self.remove(path)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Errno> {
        if from.is_empty() || to.is_empty() {
            return Err(errno::EPERM);
        }
        let moving = self.info_of(from)?;
        if from == to {
            return Ok(());
        }
        if to.starts_with(from) && to.as_bytes().get(from.len()) == Some(&b'/') {
            return Err(errno::EINVAL);
        }
        // POSIX: a file replaces a file, a directory an empty directory.
        let replace = match self.info_of(to) {
            Ok(there) => match (moving.is_dir(), there.is_dir()) {
                (false, false) => true,
                (true, true) => {
                    self.rmdir(to)?;
                    false
                }
                (false, true) => return Err(errno::EISDIR),
                (true, false) => return Err(errno::ENOTDIR),
            },
            Err(errno::ENOENT) => false,
            Err(error) => return Err(error),
        };
        self.release(from);
        self.release(to);
        self.run(false, |s| s.client()?.rename(from, to, replace))?;
        self.cache.removed(from);
        self.cache.removed(to);
        self.cache.drop_listing(split(to).0);
        self.inodes.rename(from, to);
        Ok(())
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<DirEnt>, Errno> {
        if !self.info_of(path)?.is_dir() {
            return Err(errno::ENOTDIR);
        }
        let now = self.now();
        let names = match self.cache.listing(path, now) {
            Some(names) => names.clone(),
            None => {
                let entries = self.run(true, |s| s.client()?.list(path))?;
                self.cache.put_listing(path, &entries, now);
                self.cache.listing(path, now).cloned().unwrap_or_default()
            }
        };
        Ok(names
            .into_iter()
            .map(|(name, dir)| DirEnt {
                ino: self.inodes.ino(&join(path, &name)),
                dir,
                name,
            })
            .collect())
    }

    fn flush(&mut self) -> Result<(), Errno> {
        self.run(false, |s| {
            for id in s.handles.writable() {
                s.client()?.flush(&id)?;
            }
            Ok(())
        })
    }

    fn statfs(&mut self) -> Result<StatFsRecord, Errno> {
        let size = self.run(true, |s| s.client()?.statfs())?;
        Ok(StatFsRecord {
            magic: MAGIC,
            block_size: size.unit,
            blocks: size.total / size.unit,
            blocks_free: size.available / size.unit,
            files: 1 << 20,
            files_free: 1 << 19,
            name_max: fused::wire::MAX_NAME as u64,
        })
    }
}
