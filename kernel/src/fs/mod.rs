//! Filesystem: a read-only FAT16 volume mounted from the boot disk.

mod fat;

pub use fat::Entry;

use alloc::string::String;
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

/// List the root directory.
pub fn list() -> Vec<Entry> {
    FS.lock().as_ref().map(|fs| fs.list()).unwrap_or_default()
}

/// Read a file's contents.
pub fn read(name: &str) -> Option<Vec<u8>> {
    FS.lock().as_ref().and_then(|fs| fs.read(name))
}

/// Read a file as text (lossily decoded).
pub fn read_text(name: &str) -> Option<String> {
    read(name).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}
