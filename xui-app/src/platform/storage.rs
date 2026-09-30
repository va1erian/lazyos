//! The Paint app's filesystem [`Storage`]: PNG bytes at a fixed path.
//!
//! The portable [`Storage`] trait takes no path, so a LazyOS paint session
//! saves to and loads from one path chosen at start-up (a file named in the
//! command line, else `$HOME/xpaint.png`). Writes are atomic — a temp file in
//! the target's directory, then a rename — and a path that is a symlink is
//! refused outright, so a save can never be redirected through a link.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use xui_paint::storage::Storage;

/// A PNG store at one path.
pub struct PngStorage {
    path: PathBuf,
}

impl PngStorage {
    /// A store backed by `path`.
    pub fn new(path: impl Into<PathBuf>) -> PngStorage {
        PngStorage { path: path.into() }
    }

    /// The path this store reads and writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A temp path next to the target, so the rename stays on one filesystem.
    fn temp_path(&self) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut name = self
            .path
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_else(|| "xpaint".into());
        name.push(format!(".tmp-{}-{n}", std::process::id()));
        self.path.with_file_name(name)
    }

    /// Whether `path` may be written: not an existing symlink and its parent
    /// directory exists.
    fn writable(&self, path: &Path) -> bool {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => false,
            Ok(_) => true,
            Err(_) => path.parent().is_some_and(|parent| parent.is_dir()),
        }
    }
}

impl Storage for PngStorage {
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        if !self.writable(&self.path) {
            return Err("refusing to write a symlink or a missing directory".to_string());
        }
        let temp = self.temp_path();
        let result = write_temp(&temp, bytes).and_then(|()| fs::rename(&temp, &self.path));
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result.map_err(|error| error.to_string())
    }

    fn load(&self) -> Option<Vec<u8>> {
        fs::read(&self.path).ok()
    }

    fn available(&self) -> bool {
        self.writable(&self.path)
    }
}

/// Writes `bytes` to `temp`, flushing and syncing before the caller renames it.
fn write_temp(temp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::File::create(temp)?;
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
}
