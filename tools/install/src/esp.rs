//! The ESP (step 6): the LazyOS directory written file by file to a
//! temporary name, synced, then renamed into place, so a power cut leaves
//! either the old file or the new one. Two ways in: the ESP Linux has
//! mounted, or the FAT32 partition of a disk image in test mode, written
//! through `fatfs` (also where test mode's `custom.cfg` lives).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// What the installer does to an ESP.
pub trait Esp {
    /// Free bytes.
    fn free_bytes(&mut self) -> Result<u64, String>;
    /// `path` (slash-separated, from the ESP root), or `None` when absent.
    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, String>;
    /// Replace `dir/name` with `bytes` through a temporary name.
    fn write_atomic(&mut self, dir: &str, name: &str, bytes: &[u8]) -> Result<(), String>;
    /// Remove `dir` and everything in it; whether it existed.
    fn remove_dir(&mut self, dir: &str) -> Result<bool, String>;
}

/// An ESP mounted by Linux at `root`.
pub struct MountedEsp {
    pub root: PathBuf,
}

impl Esp for MountedEsp {
    fn free_bytes(&mut self) -> Result<u64, String> {
        free_bytes(&self.root)
    }

    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, String> {
        match crate::payload::find(&self.root, path) {
            Some(file) => std::fs::read(&file)
                .map(Some)
                .map_err(|e| format!("{}: {e}", file.display())),
            None => Ok(None),
        }
    }

    fn write_atomic(&mut self, dir: &str, name: &str, bytes: &[u8]) -> Result<(), String> {
        let dir = self.root.join(dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let temp = dir.join(format!("{name}.new"));
        let target = dir.join(name);
        let failed = |e: io::Error| format!("{}: {e}", temp.display());
        let mut file = File::create(&temp).map_err(failed)?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(failed)?;
        drop(file);
        std::fs::rename(&temp, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        sync_dir(&dir)
    }

    fn remove_dir(&mut self, dir: &str) -> Result<bool, String> {
        let Some(path) = crate::payload::find(&self.root, dir) else {
            return Ok(false);
        };
        std::fs::remove_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        sync_dir(&self.root)?;
        Ok(true)
    }
}

#[cfg(unix)]
fn free_bytes(path: &std::path::Path) -> Result<u64, String> {
    crate::linux::free_bytes(path)
}

#[cfg(not(unix))]
fn free_bytes(_: &std::path::Path) -> Result<u64, String> {
    Err("a mounted ESP is a Linux feature; use --image".into())
}

/// Make a rename durable: sync the directory that holds it (Linux; FAT
/// drivers accept it).
fn sync_dir(dir: &std::path::Path) -> Result<(), String> {
    if cfg!(unix) {
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| format!("sync {}: {e}", dir.display()))?;
    }
    Ok(())
}

/// The ESP of a disk image: a byte window of the image file.
pub struct ImageEsp {
    pub path: PathBuf,
    pub start: u64,
    pub len: u64,
}

/// `[start, start + len)` of a file as a seekable stream, for `fatfs`.
pub struct Window {
    file: File,
    start: u64,
    len: u64,
    at: u64,
}

impl Read for Window {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let room = (self.len - self.at.min(self.len)) as usize;
        let take = buf.len().min(room);
        self.file.seek(SeekFrom::Start(self.start + self.at))?;
        let got = self.file.read(&mut buf[..take])?;
        self.at += got as u64;
        Ok(got)
    }
}

impl Write for Window {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let room = (self.len - self.at.min(self.len)) as usize;
        if room == 0 && !buf.is_empty() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "past the ESP"));
        }
        let take = buf.len().min(room);
        self.file.seek(SeekFrom::Start(self.start + self.at))?;
        let put = self.file.write(&buf[..take])?;
        self.at += put as u64;
        Ok(put)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for Window {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let at = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.at.checked_add_signed(delta),
        };
        self.at = at
            .filter(|at| *at <= self.len)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek outside the ESP"))?;
        Ok(self.at)
    }
}

impl Window {
    /// `[start, start + len)` of `file`.
    pub fn new(file: File, start: u64, len: u64) -> Window {
        Window {
            file,
            start,
            len,
            at: 0,
        }
    }
}

type Fs = fatfs::FileSystem<Window>;

impl ImageEsp {
    /// The ESP's filesystem. Queries open the image read-only: `fatfs` writes
    /// its FSInfo sector back when it is dropped, and nothing may be written
    /// before step 5 (that write then fails, unseen).
    fn open_with(&self, writable: bool) -> Result<Fs, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(&self.path)
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        let window = Window::new(file, self.start, self.len);
        fatfs::FileSystem::new(window, fatfs::FsOptions::new())
            .map_err(|e| format!("the image's ESP: {e}"))
    }

    fn open(&self) -> Result<Fs, String> {
        self.open_with(false)
    }

    fn open_rw(&self) -> Result<Fs, String> {
        self.open_with(true)
    }

    /// The volume serial GRUB knows the ESP by.
    pub fn serial(&self) -> Result<String, String> {
        let id = self.open()?.volume_id();
        Ok(format!("{:04X}-{:04X}", id >> 16, id & 0xFFFF))
    }

    /// Write `path` (creating its directory) in place; test mode's
    /// `custom.cfg`.
    pub fn write_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        self.write_atomic(dir, name, bytes)
    }
}

fn fat_error(what: &str) -> impl Fn(io::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// Write `bytes` as `name` in `at`, created (or truncated) in place.
fn write_new(at: &fatfs::Dir<'_, Window>, name: &str, bytes: &[u8]) -> Result<(), String> {
    let mut file = at.create_file(name).map_err(fat_error(name))?;
    file.truncate().map_err(fat_error(name))?;
    file.write_all(bytes)
        .and_then(|()| file.flush())
        .map_err(fat_error(name))
}

fn create_dirs<'a>(fs: &'a Fs, dir: &str) -> Result<fatfs::Dir<'a, Window>, String> {
    let mut at = fs.root_dir();
    for part in dir.split('/').filter(|p| !p.is_empty()) {
        at = match at.open_dir(part) {
            Ok(next) => next,
            Err(_) => at.create_dir(part).map_err(fat_error(part))?,
        };
    }
    Ok(at)
}

impl Esp for ImageEsp {
    fn free_bytes(&mut self) -> Result<u64, String> {
        let fs = self.open()?;
        let stats = fs.stats().map_err(fat_error("ESP stats"))?;
        Ok(u64::from(stats.free_clusters()) * u64::from(stats.cluster_size()))
    }

    fn read(&mut self, path: &str) -> Result<Option<Vec<u8>>, String> {
        let fs = self.open()?;
        let Ok(mut file) = fs.root_dir().open_file(path) else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(fat_error(path))?;
        Ok(Some(bytes))
    }

    fn write_atomic(&mut self, dir: &str, name: &str, bytes: &[u8]) -> Result<(), String> {
        let fs = self.open_rw()?;
        {
            let at = create_dirs(&fs, dir)?;
            let temp = format!("{name}.new");
            write_new(&at, &temp, bytes)?;
            // fatfs will not rename over a file, has no atomic replace, and
            // its rename is not transactional either. So the old bytes are
            // kept in memory, and if the new file cannot take the name they
            // are written back as a fresh file: recovery never depends on a
            // second rename. A failed recovery reports both errors.
            let previous = match at.open_file(name) {
                Ok(mut file) => {
                    let mut old = Vec::new();
                    file.read_to_end(&mut old).map_err(fat_error(name))?;
                    Some(old)
                }
                Err(_) => None,
            };
            if previous.is_some() {
                at.remove(name).map_err(fat_error(name))?;
            }
            if let Err(error) = at.rename(&temp, &at, name) {
                let error = fat_error(name)(error);
                if let Some(old) = previous {
                    if let Err(restore) = write_new(&at, name, &old) {
                        return Err(format!("{error}; restoring the old {name}: {restore}"));
                    }
                }
                return Err(error);
            }
        }
        fs.unmount().map_err(fat_error("unmount the ESP"))
    }

    fn remove_dir(&mut self, dir: &str) -> Result<bool, String> {
        let fs = self.open_rw()?;
        let found = {
            let Ok(at) = fs.root_dir().open_dir(dir) else {
                return Ok(false);
            };
            let names: Vec<String> = at
                .iter()
                .flatten()
                .map(|entry| entry.file_name())
                .filter(|name| name != "." && name != "..")
                .collect();
            for name in names {
                at.remove(&name).map_err(fat_error(&name))?;
            }
            fs.root_dir().remove(dir).map_err(fat_error(dir))?;
            true
        };
        fs.unmount().map_err(fat_error("unmount the ESP"))?;
        Ok(found)
    }
}
