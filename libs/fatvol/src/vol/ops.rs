//! The tree operations of a [`Volume`]: lookups, file I/O, creation,
//! removal, rename and listing, in terms of paths. The [`FuseFs`] methods
//! in the parent module call these and write the cache back afterwards.

use super::*;

/// `"a/b/c"` is `("a/b", "c")`; `"c"` is `("", "c")`.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        String::from(name)
    } else {
        format!("{dir}/{name}")
    }
}

/// Whether `path` is `dir` or lies below it, comparing names the way the
/// format does (case-insensitively): `Docs/x` is inside `docs`.
fn inside(path: &str, dir: &str) -> bool {
    let mut below = path.split('/');
    dir.split('/')
        .all(|part| below.next().is_some_and(|name| same_name(name, part)))
}

fn same_name(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

impl<D: BlockIo> Volume<D> {
    /// The entry `path` names, with its metadata.
    pub(super) fn find(&mut self, path: &str) -> Result<Found, Errno> {
        if path.is_empty() {
            return Ok(Found {
                dir: true,
                size: 0,
                read_only: false,
                mtime: 0,
                atime: 0,
                ctime: 0,
            });
        }
        let (parent, name) = split(path);
        let fs = self.fs()?;
        let dir = Self::open_dir(fs, parent)?;
        for entry in dir.iter().take(MAX_DIR_ENTRIES) {
            let entry = entry.map_err(to_errno)?;
            if same_name(&entry.file_name(), name) {
                let attributes = entry.attributes();
                return Ok(Found {
                    dir: entry.is_dir(),
                    size: entry.len(),
                    read_only: attributes.contains(FileAttributes::READ_ONLY),
                    mtime: to_unix(entry.modified()),
                    atime: date_to_unix(entry.accessed()),
                    ctime: to_unix(entry.created()),
                });
            }
        }
        Err(errno::ENOENT)
    }

    pub(super) fn attr(&mut self, path: &str, found: &Found) -> Attr {
        let (kind, mut permissions) = if found.dir {
            (S_IFDIR, self.opts.dir_mode)
        } else {
            (S_IFREG, self.opts.file_mode)
        };
        if found.read_only || !self.opts.writable {
            permissions &= !0o222;
        }
        Attr {
            ino: self.inodes.ino(path),
            generation: 1,
            mode: kind | u64::from(permissions),
            uid: u64::from(self.opts.uid),
            gid: u64::from(self.opts.gid),
            size: found.size,
            atime: found.atime,
            mtime: found.mtime,
            ctime: found.ctime,
        }
    }

    pub(super) fn stat(&mut self, path: &str) -> Result<Attr, Errno> {
        let found = self.find(path)?;
        Ok(self.attr(path, &found))
    }

    pub(super) fn open_file<'a>(fs: &'a Fs<D>, path: &str) -> Result<Handle<'a, D>, Errno> {
        fs.root_dir().open_file(path).map_err(to_errno)
    }

    /// Write `count` zero bytes at the end of `file`.
    pub(super) fn extend(file: &mut Handle<'_, D>, count: u64) -> Result<(), Errno> {
        let zeros = alloc::vec![0u8; ZERO_CHUNK];
        let mut left = count;
        file.seek(SeekFrom::End(0)).map_err(to_errno)?;
        while left > 0 {
            let piece = usize::try_from(left).map_or(ZERO_CHUNK, |n| n.min(ZERO_CHUNK));
            file.write_all(&zeros[..piece]).map_err(to_errno)?;
            left -= piece as u64;
        }
        Ok(())
    }

    pub(super) fn read_file(
        &mut self,
        path: &str,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, Errno> {
        let fs = self.fs()?;
        let mut file = Self::open_file(fs, path)?;
        let size = file.seek(SeekFrom::End(0)).map_err(to_errno)?;
        if offset >= size {
            return Ok(0);
        }
        file.seek(SeekFrom::Start(offset)).map_err(to_errno)?;
        let want = usize::try_from(size - offset).map_or(out.len(), |n| n.min(out.len()));
        let mut done = 0;
        while done < want {
            match file.read(&mut out[done..want]).map_err(to_errno)? {
                0 => break,
                n => done += n,
            }
        }
        Ok(done)
    }

    /// The order the operation about to run writes in (`io::order`), kept
    /// until its write-back.
    fn write_order(&self, order: Order) {
        self.cache.0.borrow_mut().set_order(order);
    }

    pub(super) fn write_file(
        &mut self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, Errno> {
        // The file's bytes, then the FAT that allocates them, then the
        // entry that names them: a cut never leaves an entry that claims
        // bytes that were not written.
        self.write_order(Order::Grow);
        let cache = self.cache.clone();
        let fs = self.fs()?;
        let mut file = Self::open_file(fs, path)?;
        let size = file.seek(SeekFrom::End(0)).map_err(to_errno)?;
        cache.0.borrow_mut().tag_file_data(true);
        let written = (|| {
            if offset > size {
                Self::extend(&mut file, offset - size)?;
            }
            file.seek(SeekFrom::Start(offset)).map_err(to_errno)?;
            file.write_all(data).map_err(to_errno)
        })();
        // Off before anything else happens, whatever came of the write: the
        // entry `file`'s flush (and its drop, on an error) writes is not file
        // data, and a tag left on would sort it before the FAT.
        cache.0.borrow_mut().tag_file_data(false);
        written?;
        file.flush().map_err(to_errno)?;
        Ok(data.len())
    }

    pub(super) fn resize(&mut self, path: &str, size: u64) -> Result<(), Errno> {
        let cache = self.cache.clone();
        let fs = self.fs()?;
        let mut file = Self::open_file(fs, path)?;
        let current = file.seek(SeekFrom::End(0)).map_err(to_errno)?;
        if size < current {
            // The entry stops naming the clusters before the FAT frees them.
            cache.0.borrow_mut().set_order(Order::Shrink);
            file.seek(SeekFrom::Start(size)).map_err(to_errno)?;
            file.truncate().map_err(to_errno)?;
        } else if size > current {
            cache.0.borrow_mut().set_order(Order::Grow);
            cache.0.borrow_mut().tag_file_data(true);
            let extended = Self::extend(&mut file, size - current);
            cache.0.borrow_mut().tag_file_data(false);
            extended?;
        }
        file.flush().map_err(to_errno)
    }

    pub(super) fn make(&mut self, path: &str, dir: bool) -> Result<Attr, Errno> {
        self.writable()?;
        match self.find(path) {
            Ok(_) => return Err(errno::EEXIST),
            Err(errno::ENOENT) => {}
            Err(code) => return Err(code),
        }
        if dir {
            // The new directory's cluster (its `.` and `..`) before the
            // entry that names it: a cut never leaves a directory whose
            // cluster still holds what a deleted file left there.
            self.write_order(Order::Grow);
        }
        {
            let fs = self.fs()?;
            let root = fs.root_dir();
            if dir {
                root.create_dir(path).map(drop).map_err(to_errno)?;
            } else {
                root.create_file(path)
                    .and_then(|mut file| file.flush())
                    .map_err(to_errno)?;
            }
        }
        self.stat(path)
    }

    pub(super) fn remove(&mut self, path: &str, want_dir: bool) -> Result<(), Errno> {
        self.writable()?;
        let found = self.find(path)?;
        if found.dir != want_dir {
            return Err(if want_dir {
                errno::ENOTDIR
            } else {
                errno::EISDIR
            });
        }
        // The entry goes before the FAT frees its clusters.
        self.write_order(Order::Shrink);
        let fs = self.fs()?;
        fs.root_dir().remove(path).map_err(to_errno)?;
        self.inodes.forget(path);
        Ok(())
    }

    pub(super) fn move_entry(&mut self, from: &str, to: &str) -> Result<(), Errno> {
        self.writable()?;
        if from.is_empty() || to.is_empty() || (inside(to, from) && !same_name(to, from)) {
            return Err(errno::EINVAL);
        }
        let source = self.find(from)?;
        if same_name(from, to) {
            // The same entry under another spelling: the library treats the
            // two names as one and does nothing, so the new case goes in by
            // way of a name that is nobody's (both steps are one operation:
            // the volume writes back only at its end).
            if from == to {
                return Ok(());
            }
            let (parent, name) = split(from);
            let (_, new_name) = split(to);
            let temp = "~lazyren.tmp";
            {
                let fs = self.fs()?;
                let dir = Self::open_dir(fs, parent)?;
                dir.rename(name, &dir, temp).map_err(to_errno)?;
                dir.rename(temp, &dir, new_name).map_err(to_errno)?;
            }
            self.inodes.rename(from, to);
            return Ok(());
        }
        {
            match self.find(to) {
                Ok(target) => {
                    if target.dir != source.dir {
                        return Err(if target.dir {
                            errno::EISDIR
                        } else {
                            errno::ENOTDIR
                        });
                    }
                    // The target is replaced, as `rename(2)` does; a
                    // directory only when it is empty (`remove` says so).
                    // Its entry goes before the FAT frees its clusters.
                    self.write_order(Order::Shrink);
                    let fs = self.fs()?;
                    fs.root_dir().remove(to).map_err(to_errno)?;
                    self.inodes.forget(to);
                }
                Err(errno::ENOENT) => {}
                Err(code) => return Err(code),
            }
        }
        let (from_parent, from_name) = split(from);
        let (to_parent, to_name) = split(to);
        {
            let fs = self.fs()?;
            let source_dir = Self::open_dir(fs, from_parent)?;
            let target_dir = Self::open_dir(fs, to_parent)?;
            source_dir
                .rename(from_name, &target_dir, to_name)
                .map_err(to_errno)?;
        }
        self.inodes.rename(from, to);
        Ok(())
    }

    pub(super) fn list(&mut self, path: &str) -> Result<Vec<DirEnt>, Errno> {
        let mut names: Vec<(String, bool)> = Vec::new();
        {
            let fs = self.fs()?;
            let dir = Self::open_dir(fs, path)?;
            for entry in dir.iter().take(MAX_DIR_ENTRIES) {
                let entry = entry.map_err(to_errno)?;
                let name = entry.file_name();
                if name != "." && name != ".." {
                    names.push((name, entry.is_dir()));
                }
            }
        }
        Ok(names
            .into_iter()
            .map(|(name, dir)| DirEnt {
                ino: self.inodes.ino(&join(path, &name)),
                dir,
                name,
            })
            .collect())
    }
}
