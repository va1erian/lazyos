//! Filesystem: a read-only FAT12/FAT16 volume mounted from the boot disk.

mod fat;

use alloc::vec::Vec;
use spin::Mutex;

static FS: Mutex<Option<fat::Fat16>> = Mutex::new(None);

/// Mount the filesystem. Returns true on success.
pub fn init() -> bool {
    match fat::Fat16::open() {
        Some(volume) => {
            *FS.lock() = Some(volume);
            true
        }
        None => false,
    }
}

/// Read a file's contents.
pub fn read(name: &str) -> Option<Vec<u8>> {
    FS.lock().as_ref().and_then(|fs| fs.read(name))
}

/// Metadata for a root-directory entry: `(size, is_dir)`.
pub fn stat(name: &str) -> Option<(u32, bool)> {
    FS.lock().as_ref().and_then(|fs| fs.stat(name))
}
