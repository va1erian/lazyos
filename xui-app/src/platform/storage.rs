//! The Paint app's filesystem [`Storage`]: PNG bytes at a fixed path.
//!
//! The portable [`Storage`] trait's path-less `save`/`load` act on one path
//! chosen at start-up (a file named in the command line, else
//! `$HOME/xpaint.png`); `save_to`/`load_from` serve the file dialogs. Writes are atomic — a temp file in
//! the target's directory, then a rename — and a path that is a symlink is
//! refused outright, so a save can never be redirected through a link.
//! [`write_atomic`] is public: LazyWriter writes its documents with it too.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xui_paint::storage::Storage;

/// A PNG store at one path.
pub struct PngStorage {
    path: PathBuf,
    max_load: u64,
}

impl PngStorage {
    /// A store backed by `path`, reading files up to [`DEFAULT_MAX_LOAD_BYTES`].
    pub fn new(path: impl Into<PathBuf>) -> PngStorage {
        PngStorage {
            path: path.into(),
            max_load: DEFAULT_MAX_LOAD_BYTES,
        }
    }

    /// The same store reading files up to `bytes`: an editor that opens
    /// layered files asks for more than a viewer of single pictures needs.
    pub fn with_max_load(mut self, bytes: u64) -> PngStorage {
        self.max_load = bytes;
        self
    }

    /// The path this store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The largest file a load reads unless the store is told otherwise; a bigger
/// one is refused unread (32 MiB, enough for a large single picture).
pub const DEFAULT_MAX_LOAD_BYTES: u64 = 32 * 1024 * 1024;

/// What the image editor asks for: a layered OpenRaster file holds one PNG per
/// layer, so it is well above what one PNG needs. The editor checks the picture
/// against its own memory budget before decoding.
pub const EDITOR_MAX_LOAD_BYTES: u64 = 256 * 1024 * 1024;

/// A temp path next to `target`, so the rename stays on one filesystem. The
/// name is only a guess at a free one: [`write_temp`] creates it exclusively.
fn temp_path(target: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = target
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "xpaint".into());
    name.push(format!(".tmp-{}-{n}", std::process::id()));
    target.with_file_name(name)
}

/// Whether `path` may be written: not an existing symlink and its parent
/// directory exists.
fn writable(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => false,
        Ok(_) => true,
        Err(_) => path.parent().is_some_and(|parent| parent.is_dir()),
    }
}

/// Writes `bytes` to `path` atomically (temp file, then rename), refusing a
/// symlink or a missing directory. Paint's saves and LazyWriter's saves and
/// exports both go through it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if !writable(path) {
        return Err("refusing to write a symlink or a missing directory".to_string());
    }
    // The name is predictable, so something may sit there already (a symlink
    // planted for the write to follow): `create_new` refuses it and the next
    // name is tried.
    let mut last = None;
    for _ in 0..TEMP_TRIES {
        let temp = temp_path(path);
        match write_temp(&temp, bytes) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => last = Some(error),
            written => {
                let result = written.and_then(|()| fs::rename(&temp, path));
                if result.is_err() {
                    let _ = fs::remove_file(&temp);
                }
                return result.map_err(|error| error.to_string());
            }
        }
    }
    Err(last.map_or_else(|| "no free temp name".to_string(), |e| e.to_string()))
}

/// How many temp names a write tries before giving up.
const TEMP_TRIES: usize = 8;

/// Reads `path`, refusing anything over `max` bytes before reading it.
fn read_bounded(path: &Path, max: u64) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > max {
        return None;
    }
    // Exactly what the file holds; `take` keeps a file that grows meanwhile
    // from going past the cap.
    let mut bytes = Vec::with_capacity(usize::try_from(len).ok()?);
    file.take(max + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= max).then_some(bytes)
}

impl Storage for PngStorage {
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        write_atomic(&self.path, bytes)
    }

    fn load(&self) -> Option<Vec<u8>> {
        read_bounded(&self.path, self.max_load)
    }

    fn available(&self) -> bool {
        writable(&self.path)
    }

    fn save_to(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        write_atomic(path, bytes)
    }

    fn load_from(&self, path: &Path) -> Option<Vec<u8>> {
        read_bounded(path, self.max_load)
    }

    fn supports_paths(&self) -> bool {
        true
    }

    fn default_path(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }
}

/// Writes `bytes` to `temp`, flushing and syncing before the caller renames it.
fn write_temp(temp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // `create_new` (O_EXCL) fails on anything already there, a symlink
    // included, instead of following it.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("xpaint-{tag}-{}-{n}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new("roundtrip");
        let storage = PngStorage::new(dir.file("a.png"));
        assert!(storage.available());
        storage.save(b"png-bytes").unwrap();
        assert_eq!(storage.load().as_deref(), Some(b"png-bytes".as_slice()));
    }

    #[test]
    fn saving_over_an_existing_file_replaces_it() {
        let dir = TempDir::new("replace");
        let storage = PngStorage::new(dir.file("a.png"));
        storage.save(b"first").unwrap();
        storage.save(b"second").unwrap();
        assert_eq!(storage.load().as_deref(), Some(b"second".as_slice()));
    }

    #[test]
    fn a_missing_directory_is_not_available() {
        let dir = TempDir::new("missing");
        let storage = PngStorage::new(dir.file("sub/a.png"));
        assert!(!storage.available());
        assert!(storage.save(b"x").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_target_is_refused_and_its_target_untouched() {
        let dir = TempDir::new("symlink");
        let real = dir.file("real.png");
        fs::write(&real, b"original").unwrap();
        let link = dir.file("link.png");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let storage = PngStorage::new(&link);
        assert!(!storage.available());
        assert!(storage.save(b"redirected").is_err());
        assert_eq!(fs::read(&real).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_save_leaves_the_previous_file_intact() {
        let dir = TempDir::new("atomic");
        let path = dir.file("a.png");
        let storage = PngStorage::new(&path);
        storage.save(b"good").unwrap();

        // Replace the target with a symlink: the save is refused before the
        // temp file is created, so the original bytes survive.
        let other = dir.file("elsewhere.png");
        fs::write(&other, b"safe").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(storage.save(b"bad").is_err());
        assert_eq!(fs::read(&other).unwrap(), b"safe");
    }

    #[test]
    fn a_load_over_the_cap_is_refused_before_it_is_read() {
        let dir = TempDir::new("cap");
        let path = dir.file("big.png");
        fs::write(&path, vec![7u8; 1000]).unwrap();
        let small = PngStorage::new(dir.file("a.png")).with_max_load(999);
        assert_eq!(small.load_from(&path), None);
        let exact = PngStorage::new(dir.file("a.png")).with_max_load(1000);
        assert_eq!(exact.load_from(&path).map(|b| b.len()), Some(1000));
        // The default is the viewer's, the editor asks for more.
        assert_eq!(DEFAULT_MAX_LOAD_BYTES, 32 * 1024 * 1024);
        assert!(EDITOR_MAX_LOAD_BYTES > DEFAULT_MAX_LOAD_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_temp_name_is_not_followed() {
        let dir = TempDir::new("planted");
        let victim = dir.file("victim.txt");
        fs::write(&victim, b"keep").unwrap();
        let temp = dir.file("a.png.tmp-planted");
        std::os::unix::fs::symlink(&victim, &temp).unwrap();
        let error = write_temp(&temp, b"redirected").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(&victim).unwrap(),
            b"keep",
            "the link was not written"
        );
        // A save still lands on a free name.
        let target = dir.file("a.png");
        write_atomic(&target, b"saved").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"saved");
    }

    #[test]
    fn path_seam_writes_and_reads_other_files() {
        let dir = TempDir::new("paths");
        let storage = PngStorage::new(dir.file("default.png"));
        assert!(storage.supports_paths());
        assert_eq!(storage.default_path(), Some(dir.file("default.png")));
        let other = dir.file("other.png");
        storage.save_to(&other, b"other").unwrap();
        assert_eq!(
            storage.load_from(&other).as_deref(),
            Some(b"other".as_slice())
        );
        assert_eq!(storage.load(), None, "the default path was not touched");
        assert!(storage.save_to(&dir.file("no/such/dir.png"), b"x").is_err());
        assert_eq!(storage.load_from(&dir.file("missing.png")), None);
    }
}
