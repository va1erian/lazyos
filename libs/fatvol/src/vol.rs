//! A mounted FAT volume as a [`fused::daemon::FuseFs`].
//!
//! Paths arrive validated and relative to the mount root. FAT has no owner
//! and no mode, so every node is reported as owned by [`Options::uid`] and
//! [`Options::gid`] with the configured modes (a file the read-only
//! attribute marks loses its write bits); `chmod` and `chown` are accepted
//! and ignored, as Linux's `vfat` does with `quiet`. Names are
//! case-insensitive and case-preserving, as the format is.
//!
//! The volume is marked clean whenever it is idle ([`Volume::settle`]): the
//! filesystem object is dropped, which writes its free-cluster count and
//! clears the dirty flag, and is opened again by the next request. A stick
//! pulled between requests is therefore a clean volume.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use alloc::rc::Rc;
use fatfs::{
    Dir, Error, File, FileAttributes, FileSystem, FsOptions, LossyOemCpConverter, Read, Seek,
    SeekFrom, Write,
};
use fused::daemon::{Errno, FuseFs, Target};
use fused::inodes::Inodes;
use fused::payload::{set, DirEnt, SetAttrRecord, StatFsRecord};
use fused::wire::{errno, Attr, S_IFDIR, S_IFREG};

use crate::clock::{date_to_unix, to_datetime, to_unix, Clock};
use crate::io::{BlockIo, Cache, Fault, Shared, Stats, SECTOR};

type Fs<D> = FileSystem<Shared<D>, Clock, LossyOemCpConverter>;
type Directory<'a, D> = Dir<'a, Shared<D>, Clock, LossyOemCpConverter>;
type Handle<'a, D> = File<'a, Shared<D>, Clock, LossyOemCpConverter>;

/// `MSDOS_SUPER_MAGIC`, what `statfs` reports for a FAT volume.
const FAT_MAGIC: u64 = 0x4d44;
/// Bytes of zeros written in one piece when a file is extended.
const ZERO_CHUNK: usize = 32 * 1024;
/// A FAT directory holds at most 65,536 entries; a longer walk is a chain
/// that loops (a hostile or damaged volume) and ends here.
const MAX_DIR_ENTRIES: usize = 1 << 16;

/// How a volume is presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub uid: u32,
    pub gid: u32,
    /// Permission bits of a directory (default `0o755`).
    pub dir_mode: u16,
    /// Permission bits of a file (default `0o644`: nothing on a stick is
    /// executable unless asked).
    pub file_mode: u16,
    /// Whether writes are allowed at all.
    pub writable: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            uid: 0,
            gid: 0,
            dir_mode: 0o755,
            file_mode: 0o644,
            writable: true,
        }
    }
}

/// Why a volume did not open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// Not a FAT volume, or its structures are inconsistent.
    NotFat,
    /// The device failed while it was read.
    Io(Fault),
}

/// What a lookup found.
#[derive(Clone, Copy, Debug)]
struct Found {
    dir: bool,
    size: u64,
    read_only: bool,
    mtime: i64,
    atime: i64,
    ctime: i64,
}

/// A FAT volume served as a directory tree.
pub struct Volume<D: BlockIo> {
    cache: Shared<D>,
    fs: Option<Fs<D>>,
    clock: Clock,
    inodes: Inodes,
    opts: Options,
    fat_bits: u8,
    id: u32,
    label: String,
    dirty_at_open: bool,
}

fn to_errno(error: Error<Fault>) -> Errno {
    match error {
        Error::NotFound => errno::ENOENT,
        Error::AlreadyExists => errno::EEXIST,
        Error::DirectoryIsNotEmpty => errno::ENOTEMPTY,
        Error::NotEnoughSpace => errno::ENOSPC,
        Error::InvalidFileNameLength => errno::ENAMETOOLONG,
        Error::UnsupportedFileNameCharacter | Error::InvalidInput => errno::EINVAL,
        Error::Io(Fault::ReadOnly) => errno::EROFS,
        Error::Io(_)
        | Error::UnexpectedEof
        | Error::WriteZero
        | Error::CorruptedFileSystem
        | _ => errno::EIO,
    }
}

fn fault_errno(fault: Fault) -> Errno {
    match fault {
        Fault::ReadOnly => errno::EROFS,
        Fault::Io | Fault::Gone | Fault::Eof => errno::EIO,
    }
}

mod ops;

impl<D: BlockIo> Volume<D> {
    /// Mount the volume that fills `dev`.
    pub fn open(dev: D, opts: Options, clock: Clock) -> Result<Volume<D>, OpenError> {
        let mut cache = Cache::new(dev, opts.writable);
        // The library trusts a boot sector more than a stick deserves.
        let mut boot = [0u8; SECTOR];
        cache.read_at(0, &mut boot).map_err(OpenError::Io)?;
        let device_sectors = cache.device().sectors();
        crate::bpb::validate(&boot, device_sectors).map_err(|_| OpenError::NotFat)?;
        let cache = Shared(Rc::new(RefCell::new(cache)));
        let fs = Self::mount(&cache, &clock)?;
        let dirty_at_open = fs.read_status_flags().map(|f| f.dirty()).unwrap_or(false);
        let label = fs
            .read_volume_label_from_root_dir()
            .ok()
            .flatten()
            .unwrap_or_else(|| fs.volume_label());
        let fat_bits = match fs.fat_type() {
            fatfs::FatType::Fat12 => 12,
            fatfs::FatType::Fat16 => 16,
            fatfs::FatType::Fat32 => 32,
        };
        Ok(Volume {
            id: fs.volume_id(),
            label: String::from(label.trim_end()),
            fat_bits,
            dirty_at_open,
            cache,
            fs: Some(fs),
            clock,
            inodes: Inodes::new(),
            opts,
        })
    }

    fn mount(cache: &Shared<D>, clock: &Clock) -> Result<Fs<D>, OpenError> {
        cache.0.borrow_mut().rewind();
        let options = FsOptions::new().time_provider(clock.clone());
        FileSystem::new(cache.clone(), options).map_err(|error| match error {
            Error::Io(fault) => OpenError::Io(fault),
            _ => OpenError::NotFat,
        })
    }

    /// The volume label (trailing spaces trimmed; `NO NAME` when unset).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The volume serial number (what `blkid` calls the UUID).
    pub fn volume_id(&self) -> u32 {
        self.id
    }

    /// 12, 16 or 32.
    pub fn fat_bits(&self) -> u8 {
        self.fat_bits
    }

    /// Whether the volume was marked dirty when it was opened (it was not
    /// cleanly removed from its last machine).
    pub fn dirty_at_open(&self) -> bool {
        self.dirty_at_open
    }

    /// The sticky device failure, if any request ever failed.
    pub fn failure(&self) -> Option<Fault> {
        self.cache.0.borrow().failure()
    }

    pub fn stats(&self) -> Stats {
        self.cache.0.borrow().stats()
    }

    /// Set the wall time stamped on files changed from now on.
    pub fn set_time(&self, unix: i64) {
        self.clock.set(unix);
    }

    /// Run `f` on the device under the cache (tests).
    pub fn with_device<R>(&self, f: impl FnOnce(&mut D) -> R) -> R {
        f(self.cache.0.borrow_mut().device_mut())
    }

    /// The volume is idle: write everything back, mark it clean and drop the
    /// filesystem object (the next request opens it again).
    pub fn settle(&mut self) -> Result<(), Fault> {
        if let Some(fs) = self.fs.take() {
            drop(fs);
        }
        self.cache.0.borrow_mut().sync()
    }

    fn fs(&mut self) -> Result<&Fs<D>, Errno> {
        if self.fs.is_none() {
            let fs = Self::mount(&self.cache, &self.clock).map_err(|error| match error {
                OpenError::Io(fault) => fault_errno(fault),
                OpenError::NotFat => errno::EIO,
            })?;
            self.fs = Some(fs);
        }
        self.fs.as_ref().ok_or(errno::EIO)
    }

    /// Write back what the operation changed; the operation's own error wins.
    fn finish<T>(&mut self, result: Result<T, Errno>) -> Result<T, Errno> {
        let flushed = self.cache.0.borrow_mut().flush_dirty();
        match (result, flushed) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(fault)) => Err(fault_errno(fault)),
            (Err(code), _) => Err(code),
        }
    }

    fn writable(&self) -> Result<(), Errno> {
        if self.opts.writable {
            Ok(())
        } else {
            Err(errno::EROFS)
        }
    }

    fn path_of(&self, target: Target) -> Result<String, Errno> {
        match target {
            Target::Path(path) => Ok(String::from(path)),
            Target::Node { ino, .. } => self
                .inodes
                .path(ino)
                .map(String::from)
                .ok_or(errno::ESTALE),
        }
    }

    fn open_dir<'a>(fs: &'a Fs<D>, path: &str) -> Result<Directory<'a, D>, Errno> {
        let root = fs.root_dir();
        if path.is_empty() {
            Ok(root)
        } else {
            root.open_dir(path).map_err(to_errno)
        }
    }
}

impl<D: BlockIo> FuseFs for Volume<D> {
    fn lookup(&mut self, target: Target) -> Result<Attr, Errno> {
        let path = self.path_of(target)?;
        self.stat(&path)
    }

    fn read(&mut self, target: Target, offset: u64, out: &mut [u8]) -> Result<usize, Errno> {
        let path = self.path_of(target)?;
        if self.find(&path)?.dir {
            return Err(errno::EISDIR);
        }
        self.read_file(&path, offset, out)
    }

    fn write(&mut self, target: Target, offset: u64, data: &[u8]) -> Result<usize, Errno> {
        let path = self.path_of(target)?;
        self.writable()?;
        let result = match self.find(&path) {
            Ok(found) if found.dir => Err(errno::EISDIR),
            Ok(_) => self.write_file(&path, offset, data),
            Err(code) => Err(code),
        };
        self.finish(result)
    }

    fn truncate(&mut self, target: Target, size: u64) -> Result<(), Errno> {
        let path = self.path_of(target)?;
        self.writable()?;
        let result = match self.find(&path) {
            Ok(found) if found.dir => Err(errno::EISDIR),
            Ok(_) => self.resize(&path, size),
            Err(code) => Err(code),
        };
        self.finish(result)
    }

    fn setattr(&mut self, path: &str, change: &SetAttrRecord) -> Result<Attr, Errno> {
        let found = self.find(path)?;
        let stamps = change.mask & (set::MTIME | set::ATIME) != 0;
        if stamps && !found.dir && self.opts.writable {
            let result = (|| {
                let fs = self.fs()?;
                let mut file = Self::open_file(fs, path)?;
                if change.mask & set::MTIME != 0 {
                    file.set_modified(to_datetime(change.mtime));
                }
                if change.mask & set::ATIME != 0 {
                    file.set_accessed(to_datetime(change.atime).date);
                }
                file.flush().map_err(to_errno)
            })();
            self.finish(result)?;
        }
        // Mode and ownership do not exist here: accepted, not stored.
        self.stat(path)
    }

    fn create(&mut self, path: &str, _mode: u16, _uid: u32, _gid: u32) -> Result<Attr, Errno> {
        let result = self.make(path, false);
        self.finish(result)
    }

    fn mkdir(&mut self, path: &str, _mode: u16, _uid: u32, _gid: u32) -> Result<Attr, Errno> {
        let result = self.make(path, true);
        self.finish(result)
    }

    fn unlink(&mut self, path: &str) -> Result<(), Errno> {
        let result = self.remove(path, false);
        self.finish(result)
    }

    fn rmdir(&mut self, path: &str) -> Result<(), Errno> {
        let result = self.remove(path, true);
        self.finish(result)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Errno> {
        let result = self.move_entry(from, to);
        self.finish(result)
    }

    fn readdir(&mut self, path: &str) -> Result<Vec<DirEnt>, Errno> {
        if !self.find(path)?.dir {
            return Err(errno::ENOTDIR);
        }
        self.list(path)
    }

    fn flush(&mut self) -> Result<(), Errno> {
        self.cache.0.borrow_mut().sync().map_err(fault_errno)
    }

    fn statfs(&mut self) -> Result<StatFsRecord, Errno> {
        let stats = self.fs()?.stats().map_err(to_errno)?;
        Ok(StatFsRecord {
            magic: FAT_MAGIC,
            block_size: u64::from(stats.cluster_size()),
            blocks: u64::from(stats.total_clusters()),
            blocks_free: u64::from(stats.free_clusters()),
            files: 0,
            files_free: 0,
            name_max: 255,
        })
    }
}
